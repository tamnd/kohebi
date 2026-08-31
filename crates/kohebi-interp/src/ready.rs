//! A compiled body with the work that does not depend on the arguments already
//! done.
//!
//! The compiler leaves a body's literals as [`Value`]s, which is the parser's
//! type rather than the runtime's. Turning one into an [`Object`] is cheap for a
//! number and not cheap for a string, where it means an allocation and a copy,
//! so it is done once for the whole run rather than on every load. Doing it once
//! is also what makes `x = "abc"` and `y = "abc"` two references to one string
//! rather than two strings, which is what `x is y` being true rests on.
//!
//! Once per run is not the same as once per call, which is the whole reason this
//! type exists. A pool built at the top of the interpreter loop would be rebuilt
//! by every call, and a function called a million times would build its pool a
//! million times. So the pools are built up front in the shape the bodies
//! already have: one per body, with the pools of the bodies defined inside it
//! hanging off it by the same index the code uses. A `def` picks its child out
//! by that index and the function object carries it, so a call has nothing left
//! to build.
//!
//! A body also carries which module it came from, as the name table its globals
//! are numbered against and the namespace those names live in between runs. A
//! function is defined in one module and can be called from another, and when
//! that happens the two things it needs are exactly these, so they travel with
//! the body rather than being looked up from wherever the call happened to be.

use std::fmt;
use std::rc::Rc;

use kohebi_bc::code::{Code, ConstId, FuncId, Module};
use kohebi_parse::Value;

use kohebi_core::{Object, Result};

use crate::module::Namespace;
use crate::vm::later;

/// One body, ready to run.
pub struct Ready {
    code: Rc<Code>,
    consts: Vec<Constant>,
    /// One per entry of `code.functions`, in the same order, so a [`FuncId`]
    /// indexes both.
    functions: Vec<Rc<Ready>>,
    /// Every global name the module this body came from mentions, which is what
    /// the [`NameId`](kohebi_bc::code::NameId)s in its instructions index.
    names: Rc<[Box<str>]>,
    /// Where those names live while that module is not the one running.
    home: Namespace,
}

impl Ready {
    /// Prepare a module and every body in it, against the namespace its globals
    /// belong to.
    #[must_use]
    pub fn new(module: &Module, home: &Namespace) -> Rc<Self> {
        Ready::body(&module.body, &module.names, home)
    }

    fn body(code: &Rc<Code>, names: &Rc<[Box<str>]>, home: &Namespace) -> Rc<Self> {
        Rc::new(Ready {
            code: Rc::clone(code),
            consts: code.consts.iter().map(convert).collect(),
            functions: code
                .functions
                .iter()
                .map(|inner| Ready::body(inner, names, home))
                .collect(),
            names: Rc::clone(names),
            home: Rc::clone(home),
        })
    }

    /// The name table this body's globals are numbered against.
    #[must_use]
    pub fn names(&self) -> &Rc<[Box<str>]> {
        &self.names
    }

    /// The namespace this body's globals belong to.
    #[must_use]
    pub fn home(&self) -> &Namespace {
        &self.home
    }

    /// The instructions and everything the compiler wrote alongside them.
    #[must_use]
    pub fn code(&self) -> &Code {
        &self.code
    }

    /// The same, shared, for a frame that has to outlive the call that made it.
    #[must_use]
    pub fn shared(&self) -> &Rc<Code> {
        &self.code
    }

    /// The body of a `def`, by the number the body holding it gave it.
    #[must_use]
    pub fn function(&self, id: FuncId) -> Option<&Rc<Ready>> {
        self.functions.get(id.0 as usize)
    }

    /// A literal, by the number the compiler gave it.
    ///
    /// # Errors
    ///
    /// A `NotImplementedError` for a literal this runtime cannot build yet,
    /// which is only raised if the program actually loads it.
    pub fn constant(&self, id: ConstId) -> Result<Object> {
        match self.consts.get(id.0 as usize) {
            Some(Constant::Value(value)) => Ok(value.clone()),
            Some(Constant::Missing(what)) => Err(later(what)),
            None => unreachable!("a literal is numbered into the body that holds it"),
        }
    }
}

impl fmt::Debug for Ready {
    /// The name of the body, because everything else in here is a listing and
    /// [`kohebi_bc::print()`] is what prints a listing.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<ready {}>", self.code.name)
    }
}

/// A literal, converted.
#[derive(Debug)]
enum Constant {
    Value(Object),
    /// One this runtime cannot build yet, named for its message. Kept here
    /// rather than refused while the pool is built, so that a program with a
    /// complex literal it never evaluates still runs.
    Missing(&'static str),
}

fn convert(value: &Value) -> Constant {
    match value {
        Value::None => Constant::Value(Object::None),
        Value::Ellipsis => Constant::Value(Object::Ellipsis),
        Value::Bool(value) => Constant::Value(Object::Bool(*value)),
        Value::Int(value) => Constant::Value(Object::Int(value.clone())),
        Value::Float(value) => Constant::Value(Object::Float(*value)),
        Value::Str(value) => Constant::Value(Object::Str(Rc::new(value.clone()))),
        Value::Bytes(value) => Constant::Value(Object::Bytes(Rc::from(&**value))),
        Value::Imaginary(_) => Constant::Missing("the complex type"),
    }
}
