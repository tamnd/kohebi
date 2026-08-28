//! interp: the half of M0.4 that is about dispatch rather than arithmetic.
//!
//! `nbody` answers what sealing is worth when the work is float arithmetic on
//! attributes of one class. The answer there turned out to be nothing, because a
//! compiler that hoists its guards out of the loop already has everything. That
//! result only holds where the guard can be hoisted, and in a tree walker it
//! cannot: `node.eval(env)` lands on a different class on almost every step, so
//! the check has to happen every time and there is no loop to lift it out of.
//! This workload is here to price that case.
//!
//! Two variants, not the four `nbody` has, and the two it drops are dropped for
//! a reason rather than for time:
//!
//! - `typed` isolates unboxing. Every value in this workload is a small integer,
//!   and small integers are already immediates in this object model, so there is
//!   no boxing to remove and the variant would be a copy of `open`.
//! - `hoisted` isolates guard hoisting. A polymorphic dispatch site cannot have
//!   its guard hoisted, which is the whole point of the workload.
//!
//! So for a tree walker, `open` against `sealed` is the sealing factor, with
//! nothing else mixed into it.
//!
//! The environment is deliberately the same code in both variants. It is a hash
//! map from interned names, matching what CPython's dict does on identifier
//! keys, and sealing does not change it. Holding it constant keeps the two
//! variants comparable with each other, and matching CPython keeps them
//! comparable with CPython.

use std::cell::UnsafeCell;
use std::rc::Rc;

use crate::name::{Name, NameMap, name_map};
use crate::shape::{Cache, SlotKind, define_shape, instance_of, load_attr};
use crate::value::{
    Obj, Value, alloc_instance, binop_add, binop_floordiv, binop_mod, binop_mul, binop_sub,
    compare_eq, compare_gt, compare_le, compare_lt,
};

// ---------------------------------------------------------------------------
// The environment, shared by both variants.
// ---------------------------------------------------------------------------

/// A scope: a map of its own bindings and a link to the enclosing one.
///
/// The parent is a raw pointer rather than a reference because a child scope is
/// created inside a call whose caller's scope is a `&mut` further down the same
/// Rust stack, and the borrow checker has no way to see that the child dies
/// first. It always does: `call` creates the scope, evaluates the body, and
/// drops it before returning.
pub struct Env<V> {
    vars: NameMap<V>,
    parent: *const Env<V>,
}

impl<V: Clone> Env<V> {
    pub fn root() -> Env<V> {
        Env {
            vars: name_map(),
            parent: std::ptr::null(),
        }
    }

    fn child(parent: &Env<V>) -> Env<V> {
        Env {
            vars: name_map(),
            parent: parent as *const Env<V>,
        }
    }

    fn get(&self, name: &Name) -> V {
        let mut env = self;
        loop {
            if let Some(v) = env.vars.get(name) {
                return v.clone();
            }
            if env.parent.is_null() {
                panic!("NameError: {}", name.as_str());
            }
            env = unsafe { &*env.parent };
        }
    }

    fn set(&mut self, name: Name, value: V) {
        self.vars.insert(name, value);
    }
}

// ---------------------------------------------------------------------------
// sealed: the class hierarchy is closed, so dispatch is a match.
// ---------------------------------------------------------------------------

// The program being interpreted does not use every operator, but an interpreter
// that only implements the operators one program happens to need would have a
// smaller dispatch table than a real one, and the size of that table is part of
// what is being measured.
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum Op {
    Add,
    Sub,
    Mul,
    FloorDiv,
    Mod,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum Cmp {
    Lt,
    Gt,
    Eq,
    Le,
}

pub struct FuncDef {
    name: Name,
    params: Vec<Name>,
    body: Node,
}

/// What a sealing compiler knows the node hierarchy to be. Eleven classes, none
/// of which can gain a subclass or have `eval` replaced, so the virtual call
/// becomes a jump table and the attribute loads become field offsets.
pub enum Node {
    Num(i64),
    Var(Name),
    Bin(Op, Box<Node>, Box<Node>),
    Comp(Cmp, Box<Node>, Box<Node>),
    Assign(Name, Box<Node>),
    If(Box<Node>, Box<Node>, Option<Box<Node>>),
    While(Box<Node>, Box<Node>),
    Block(Vec<Node>),
    Func(Rc<FuncDef>),
    Call(Name, Vec<Node>),
    Ret(Box<Node>),
}

/// The two things a binding can hold in this program. A sealed build knows the
/// set is closed and can use a tag rather than a pointer to a heap object.
#[derive(Clone)]
pub enum Val {
    Int(i64),
    Func(Rc<FuncDef>),
}

impl Val {
    #[inline(always)]
    fn int(&self) -> i64 {
        match self {
            Val::Int(i) => *i,
            Val::Func(_) => panic!("TypeError: function in arithmetic"),
        }
    }

    #[inline(always)]
    fn truthy(&self) -> bool {
        match self {
            Val::Int(i) => *i != 0,
            Val::Func(_) => true,
        }
    }
}

/// `Err` is a `return` in flight. Python raises an exception to unwind out of
/// nested `eval` frames, and `?` is the same control flow without the traceback
/// machinery, which is a place the Rust side is cheaper than CPython by more
/// than sealing accounts for. Called out in the writeup rather than hidden.
type Flow<T> = Result<T, T>;

impl Node {
    fn eval(&self, env: &mut Env<Val>) -> Flow<Val> {
        Ok(match self {
            Node::Num(n) => Val::Int(*n),
            Node::Var(n) => env.get(n),
            Node::Bin(op, l, r) => {
                let a = l.eval(env)?.int();
                let b = r.eval(env)?.int();
                // Python integers do not wrap, so the checks are the deopt edge
                // a real emitter would guard with. They never fire here.
                Val::Int(match op {
                    Op::Add => a.checked_add(b).expect("int overflow"),
                    Op::Sub => a.checked_sub(b).expect("int overflow"),
                    Op::Mul => a.checked_mul(b).expect("int overflow"),
                    Op::FloorDiv => a.div_euclid(b),
                    Op::Mod => a.rem_euclid(b),
                })
            }
            Node::Comp(op, l, r) => {
                let a = l.eval(env)?.int();
                let b = r.eval(env)?.int();
                let t = match op {
                    Cmp::Lt => a < b,
                    Cmp::Gt => a > b,
                    Cmp::Eq => a == b,
                    Cmp::Le => a <= b,
                };
                Val::Int(t as i64)
            }
            Node::Assign(n, e) => {
                let v = e.eval(env)?;
                env.set(n.clone(), v.clone());
                v
            }
            Node::If(test, then, orelse) => {
                if test.eval(env)?.truthy() {
                    then.eval(env)?
                } else if let Some(o) = orelse {
                    o.eval(env)?
                } else {
                    Val::Int(0)
                }
            }
            Node::While(test, body) => {
                let mut result = Val::Int(0);
                while test.eval(env)?.truthy() {
                    result = body.eval(env)?;
                }
                result
            }
            Node::Block(stmts) => {
                let mut result = Val::Int(0);
                for s in stmts {
                    result = s.eval(env)?;
                }
                result
            }
            Node::Func(f) => {
                env.set(f.name.clone(), Val::Func(f.clone()));
                Val::Int(0)
            }
            Node::Call(name, args) => {
                let f = match env.get(name) {
                    Val::Func(f) => f,
                    Val::Int(_) => panic!("TypeError: int is not callable"),
                };
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(a.eval(env)?);
                }
                call_sealed(&f, vals, env)?
            }
            Node::Ret(e) => return Err(e.eval(env)?),
        })
    }
}

fn call_sealed(f: &FuncDef, args: Vec<Val>, env: &Env<Val>) -> Flow<Val> {
    let mut local = Env::child(env);
    for (p, a) in f.params.iter().zip(args) {
        local.set(p.clone(), a);
    }
    match f.body.eval(&mut local) {
        Ok(_) => Ok(Val::Int(0)),
        // The `return` lands here, which is where Python's `except Return`
        // catches it.
        Err(v) => Ok(v),
    }
}

fn program_sealed() -> Node {
    let n = Name::new("n");
    let fib = Name::new("fib");
    let total = Name::new("total");
    let i = Name::new("i");

    let body = Node::Block(vec![Node::If(
        Box::new(Node::Comp(
            Cmp::Lt,
            Box::new(Node::Var(n.clone())),
            Box::new(Node::Num(2)),
        )),
        Box::new(Node::Ret(Box::new(Node::Var(n.clone())))),
        Some(Box::new(Node::Ret(Box::new(Node::Bin(
            Op::Add,
            Box::new(Node::Call(
                fib.clone(),
                vec![Node::Bin(
                    Op::Sub,
                    Box::new(Node::Var(n.clone())),
                    Box::new(Node::Num(1)),
                )],
            )),
            Box::new(Node::Call(
                fib.clone(),
                vec![Node::Bin(
                    Op::Sub,
                    Box::new(Node::Var(n.clone())),
                    Box::new(Node::Num(2)),
                )],
            )),
        ))))),
    )]);

    Node::Block(vec![
        Node::Func(Rc::new(FuncDef {
            name: fib.clone(),
            params: vec![n],
            body,
        })),
        Node::Assign(total.clone(), Box::new(Node::Num(0))),
        Node::Assign(i.clone(), Box::new(Node::Num(0))),
        Node::While(
            Box::new(Node::Comp(
                Cmp::Lt,
                Box::new(Node::Var(i.clone())),
                Box::new(Node::Num(LIMIT)),
            )),
            Box::new(Node::Block(vec![
                Node::Assign(
                    total.clone(),
                    Box::new(Node::Bin(
                        Op::Add,
                        Box::new(Node::Var(total.clone())),
                        Box::new(Node::Call(fib, vec![Node::Var(i.clone())])),
                    )),
                ),
                Node::Assign(
                    i.clone(),
                    Box::new(Node::Bin(
                        Op::Add,
                        Box::new(Node::Var(i.clone())),
                        Box::new(Node::Num(1)),
                    )),
                ),
            ])),
        ),
        Node::Var(total),
    ])
}

/// Matches `workloads/interp.py`. Changing it changes the answer, so it is one
/// constant in one place.
const LIMIT: i64 = 28;

pub fn sealed(iterations: usize) -> i64 {
    let tree = program_sealed();
    let mut result = 0;
    for _ in 0..iterations {
        let mut env = Env::root();
        result = match tree.eval(&mut env) {
            Ok(v) => v.int(),
            Err(v) => v.int(),
        };
    }
    result
}

// ---------------------------------------------------------------------------
// open: nodes are instances, dispatch goes through the shape.
// ---------------------------------------------------------------------------

const A_VALUE: u32 = 0;
const A_NAME: u32 = 1;
const A_OP: u32 = 2;
const A_LEFT: u32 = 3;
const A_RIGHT: u32 = 4;
const A_EXPR: u32 = 5;
const A_TEST: u32 = 6;
const A_THEN: u32 = 7;
const A_ORELSE: u32 = 8;
const A_BODY: u32 = 9;
const A_STMTS: u32 = 10;
const A_PARAMS: u32 = 11;
const A_ARGS: u32 = 12;

/// One inline cache per site in the interpreter's own source, which is where a
/// real runtime puts them: attached to the code object of `BinOp.eval`, not to
/// each `BinOp` node. Every one of them is monomorphic and hits, because
/// `self.left` inside `BinOp.eval` is only ever reached on a `BinOp`. What stays
/// polymorphic is the dispatch, and that is the point.
#[derive(Default)]
struct Sites {
    num_value: Cache,
    var_name: Cache,
    bin_op: Cache,
    bin_left: Cache,
    bin_right: Cache,
    cmp_op: Cache,
    cmp_left: Cache,
    cmp_right: Cache,
    assign_name: Cache,
    assign_expr: Cache,
    if_test: Cache,
    if_then: Cache,
    if_orelse: Cache,
    while_test: Cache,
    while_body: Cache,
    block_stmts: Cache,
    func_name: Cache,
    func_params: Cache,
    func_body: Cache,
    call_name: Cache,
    call_args: Cache,
    ret_expr: Cache,
}

struct Shapes {
    num: u32,
    var: u32,
    bin: u32,
    cmp: u32,
    assign: u32,
    iff: u32,
    whil: u32,
    block: u32,
    func: u32,
    call: u32,
    ret: u32,
}

fn shapes() -> Shapes {
    use SlotKind::{Int, Ref};
    Shapes {
        num: define_shape("Num", &[(A_VALUE, Int)]),
        var: define_shape("Var", &[(A_NAME, Ref)]),
        bin: define_shape("BinOp", &[(A_OP, Int), (A_LEFT, Ref), (A_RIGHT, Ref)]),
        cmp: define_shape("Compare", &[(A_OP, Int), (A_LEFT, Ref), (A_RIGHT, Ref)]),
        assign: define_shape("Assign", &[(A_NAME, Ref), (A_EXPR, Ref)]),
        iff: define_shape("If", &[(A_TEST, Ref), (A_THEN, Ref), (A_ORELSE, Ref)]),
        whil: define_shape("While", &[(A_TEST, Ref), (A_BODY, Ref)]),
        block: define_shape("Block", &[(A_STMTS, Ref)]),
        func: define_shape(
            "Func",
            &[(A_NAME, Ref), (A_PARAMS, Ref), (A_BODY, Ref)],
        ),
        call: define_shape("Call", &[(A_NAME, Ref), (A_ARGS, Ref)]),
        ret: define_shape("Ret", &[(A_EXPR, Ref)]),
    }
}

fn str_value(name: &Name) -> Value {
    Value::from_obj(Obj::Str(name.clone()))
}

fn list_value(items: Vec<Value>) -> Value {
    Value::from_obj(Obj::List(UnsafeCell::new(items)))
}

#[inline(always)]
fn as_name(v: &Value) -> &Name {
    match v.as_obj() {
        Obj::Str(n) => n,
        _ => panic!("expected a name"),
    }
}

#[inline(always)]
fn as_list(v: &Value) -> &Vec<Value> {
    match v.as_obj() {
        Obj::List(l) => unsafe { &*l.get() },
        _ => panic!("expected a list"),
    }
}

/// The dispatch a `--open` build is left with: read the shape off the object,
/// index a table, call through the pointer. Nothing about the receiver is known
/// at the call site, so there is nothing to inline and nothing to hoist.
fn eval_open(node: &Value, env: &mut Env<Value>, s: &Sites, sh: &Shapes) -> Flow<Value> {
    let shape = instance_of(node).shape;
    if shape == sh.num {
        Ok(load_attr(node, A_VALUE, &s.num_value))
    } else if shape == sh.var {
        let name = load_attr(node, A_NAME, &s.var_name);
        Ok(env.get(as_name(&name)))
    } else if shape == sh.bin {
        let op = load_attr(node, A_OP, &s.bin_op).as_int();
        let left = load_attr(node, A_LEFT, &s.bin_left);
        let a = eval_open(&left, env, s, sh)?;
        let right = load_attr(node, A_RIGHT, &s.bin_right);
        let b = eval_open(&right, env, s, sh)?;
        Ok(match op {
            0 => binop_add(&a, &b),
            1 => binop_sub(&a, &b),
            2 => binop_mul(&a, &b),
            3 => binop_floordiv(&a, &b),
            _ => binop_mod(&a, &b),
        })
    } else if shape == sh.cmp {
        let op = load_attr(node, A_OP, &s.cmp_op).as_int();
        let left = load_attr(node, A_LEFT, &s.cmp_left);
        let a = eval_open(&left, env, s, sh)?;
        let right = load_attr(node, A_RIGHT, &s.cmp_right);
        let b = eval_open(&right, env, s, sh)?;
        Ok(Value::from_bool(match op {
            0 => compare_lt(&a, &b),
            1 => compare_gt(&a, &b),
            2 => compare_eq(&a, &b),
            _ => compare_le(&a, &b),
        }))
    } else if shape == sh.assign {
        let expr = load_attr(node, A_EXPR, &s.assign_expr);
        let v = eval_open(&expr, env, s, sh)?;
        let name = load_attr(node, A_NAME, &s.assign_name);
        env.set(as_name(&name).clone(), v.clone());
        Ok(v)
    } else if shape == sh.iff {
        let test = load_attr(node, A_TEST, &s.if_test);
        if eval_open(&test, env, s, sh)?.truthy() {
            let then = load_attr(node, A_THEN, &s.if_then);
            return eval_open(&then, env, s, sh);
        }
        let orelse = load_attr(node, A_ORELSE, &s.if_orelse);
        if orelse.is_obj() {
            return eval_open(&orelse, env, s, sh);
        }
        Ok(Value::from_int(0))
    } else if shape == sh.whil {
        let test = load_attr(node, A_TEST, &s.while_test);
        let body = load_attr(node, A_BODY, &s.while_body);
        let mut result = Value::from_int(0);
        while eval_open(&test, env, s, sh)?.truthy() {
            result = eval_open(&body, env, s, sh)?;
        }
        Ok(result)
    } else if shape == sh.block {
        let stmts = load_attr(node, A_STMTS, &s.block_stmts);
        let mut result = Value::from_int(0);
        for stmt in as_list(&stmts) {
            result = eval_open(stmt, env, s, sh)?;
        }
        Ok(result)
    } else if shape == sh.func {
        let name = load_attr(node, A_NAME, &s.func_name);
        env.set(as_name(&name).clone(), node.clone());
        Ok(Value::from_int(0))
    } else if shape == sh.call {
        let name = load_attr(node, A_NAME, &s.call_name);
        let func = env.get(as_name(&name));
        let args = load_attr(node, A_ARGS, &s.call_args);
        let arg_list = as_list(&args);
        let mut vals = Vec::with_capacity(arg_list.len());
        for a in arg_list {
            vals.push(eval_open(a, env, s, sh)?);
        }
        call_open(&func, vals, env, s, sh)
    } else if shape == sh.ret {
        let expr = load_attr(node, A_EXPR, &s.ret_expr);
        Err(eval_open(&expr, env, s, sh)?)
    } else {
        panic!("no eval for shape {shape}")
    }
}

fn call_open(
    func: &Value,
    args: Vec<Value>,
    env: &Env<Value>,
    s: &Sites,
    sh: &Shapes,
) -> Flow<Value> {
    let mut local = Env::child(env);
    let params = load_attr(func, A_PARAMS, &s.func_params);
    for (p, a) in as_list(&params).iter().zip(args) {
        local.set(as_name(p).clone(), a);
    }
    let body = load_attr(func, A_BODY, &s.func_body);
    match eval_open(&body, &mut local, s, sh) {
        Ok(_) => Ok(Value::from_int(0)),
        Err(v) => Ok(v),
    }
}

fn program_open(sh: &Shapes) -> Value {
    let n = str_value(&Name::new("n"));
    let fib = str_value(&Name::new("fib"));
    let total = str_value(&Name::new("total"));
    let i = str_value(&Name::new("i"));

    // An `Int` slot holds the bare `i64`, not a tagged word. That is the whole
    // point of a typed slot, and passing a tagged one here made `Num(2)`
    // evaluate to 17.
    let num = |v: i64| alloc_instance(sh.num, &[v as u64]);
    let var = |name: &Value| alloc_instance(sh.var, &[name.clone().into_raw()]);
    let bin = |op: i64, l: Value, r: Value| {
        alloc_instance(sh.bin, &[op as u64, l.into_raw(), r.into_raw()])
    };
    let cmp = |op: i64, l: Value, r: Value| {
        alloc_instance(sh.cmp, &[op as u64, l.into_raw(), r.into_raw()])
    };
    let assign = |name: &Value, e: Value| {
        alloc_instance(sh.assign, &[name.clone().into_raw(), e.into_raw()])
    };
    let ret = |e: Value| alloc_instance(sh.ret, &[e.into_raw()]);
    let block = |stmts: Vec<Value>| alloc_instance(sh.block, &[list_value(stmts).into_raw()]);
    let call = |name: &Value, args: Vec<Value>| {
        alloc_instance(
            sh.call,
            &[name.clone().into_raw(), list_value(args).into_raw()],
        )
    };

    let body = block(vec![alloc_instance(
        sh.iff,
        &[
            cmp(0, var(&n), num(2)).into_raw(),
            ret(var(&n)).into_raw(),
            ret(bin(
                0,
                call(&fib, vec![bin(1, var(&n), num(1))]),
                call(&fib, vec![bin(1, var(&n), num(2))]),
            ))
            .into_raw(),
        ],
    )]);

    let fib_def = alloc_instance(
        sh.func,
        &[
            fib.clone().into_raw(),
            list_value(vec![n]).into_raw(),
            body.into_raw(),
        ],
    );

    block(vec![
        fib_def,
        assign(&total, num(0)),
        assign(&i, num(0)),
        alloc_instance(
            sh.whil,
            &[
                cmp(0, var(&i), num(LIMIT)).into_raw(),
                block(vec![
                    assign(&total, bin(0, var(&total), call(&fib, vec![var(&i)]))),
                    assign(&i, bin(0, var(&i), num(1))),
                ])
                .into_raw(),
            ],
        ),
        var(&total),
    ])
}

pub fn open(iterations: usize) -> i64 {
    let sh = shapes();
    let sites = Sites::default();
    let tree = program_open(&sh);
    let mut result = Value::from_int(0);
    for _ in 0..iterations {
        let mut env = Env::root();
        result = match eval_open(&tree, &mut env, &sites, &sh) {
            Ok(v) => v,
            Err(v) => v,
        };
    }
    result.as_int()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `workloads/interp.py` prints: the sum of fib(0..27).
    const EXPECTED: i64 = 514_228;

    #[test]
    fn both_variants_agree_with_python() {
        assert_eq!(sealed(1), EXPECTED, "sealed");
        assert_eq!(open(1), EXPECTED, "open");
    }

    /// A guard that only ever misses once per site is a guard the experiment is
    /// not really paying for. These have to hit, or `open` is measuring cache
    /// thrash instead of dispatch.
    #[test]
    fn the_attribute_caches_are_monomorphic() {
        let sh = shapes();
        let sites = Sites::default();
        let tree = program_open(&sh);
        let mut env = Env::root();
        let _ = eval_open(&tree, &mut env, &sites, &sh);
        assert_eq!(sites.bin_left.misses.get(), 1);
        assert_eq!(sites.var_name.misses.get(), 1);
        assert_eq!(sites.call_args.misses.get(), 1);
    }
}
