//! Functions defined in Python, and what a call does to their arguments.
//!
//! A function object is its code, its name, and the defaults the `def`
//! evaluated. The defaults belong here rather than to the code because they are
//! values rather than instructions: `def f(x=[])` runs the `[]` once, where the
//! `def` is, and every call that leaves `x` out gets that same list. A `def`
//! inside a loop builds a new function every turn out of the same code, which is
//! the same fact from the other side.
//!
//! ## Binding
//!
//! [`Function::bind`] turns a call's arguments into the registers the body
//! starts with. The parameters are the low registers in a fixed order, so
//! binding is filling in a vector from zero rather than a lookup per parameter,
//! and only a keyword argument costs a search.
//!
//! Almost all of the code here is the failures. There are seven ways to call a
//! function wrongly and Python has a different sentence for each of them, down
//! to whether the list at the end has an Oxford comma in it. They are worth
//! matching exactly, because a `TypeError` from a call is something people read
//! far more often than they read a traceback.

use std::any::Any;
use std::fmt;
use std::rc::Rc;

use kohebi_bc::code::{Code, Reg};
use kohebi_core::dict::Dict;
use kohebi_core::{Error, Native, Object, Result, ops};

use crate::ready::Ready;

/// A function defined in Python.
pub struct Function {
    ready: Rc<Ready>,
    /// Defaults for the trailing positional parameters, evaluated at `def` time.
    defaults: Vec<Object>,
    /// Defaults for the keyword-only ones, with a hole where there is none.
    kw_defaults: Vec<Option<Object>>,
    /// The cells this function closed over, in the order the body's free
    /// registers take them. Shared with the frame that defined it, which is
    /// what makes it a closure rather than a copy.
    captures: Vec<Object>,
}

impl Function {
    /// A function object, as a `def` or a `lambda` leaves it.
    #[must_use]
    pub fn new(
        ready: Rc<Ready>,
        defaults: Vec<Object>,
        kw_defaults: Vec<Option<Object>>,
        captures: Vec<Object>,
    ) -> Self {
        Function {
            ready,
            defaults,
            kw_defaults,
            captures,
        }
    }

    /// The body a call runs, with its constants already built.
    #[must_use]
    pub fn ready(&self) -> &Rc<Ready> {
        &self.ready
    }

    /// What the compiler wrote for that body.
    #[must_use]
    pub fn code(&self) -> &Code {
        self.ready.code()
    }

    /// The registers the body starts with, filled in from a call's arguments.
    ///
    /// The positional arguments arrive as an iterator rather than a vector so
    /// that the only allocation a call makes is the registers themselves. They
    /// are read straight out of the caller's frame into the callee's, and the
    /// leftovers a `*args` collects are the one case that needs somewhere else
    /// to put them.
    ///
    /// # Errors
    ///
    /// A `TypeError` for every way a call can fail to match the parameters.
    /// Reading the arguments can also fail, on a register the caller never
    /// wrote, which is a compiler bug rather than a program one. Nothing else
    /// can go wrong here, because nothing in binding runs user code: a default
    /// was already evaluated by the `def`.
    #[expect(
        clippy::too_many_lines,
        reason = "the argument protocol is one function in CPython too, and \
                  every branch here is a different sentence it has to say"
    )]
    pub fn bind(
        &self,
        positional_args: impl ExactSizeIterator<Item = Result<Object>>,
        by_name: Vec<(Box<str>, Object)>,
    ) -> Result<Vec<Option<Object>>> {
        let code = self.code();
        let shape = code.params;
        let positional = shape.positional as usize;
        let keyword_only = shape.keyword_only as usize;
        let name = |at: usize| code.local_at(Reg(u32::try_from(at).unwrap_or(u32::MAX)));
        let star_slot = positional;
        let keyword_slot = positional + usize::from(shape.star);
        let double_star_slot = keyword_slot + keyword_only;

        let mut registers: Vec<Option<Object>> = vec![None; code.registers as usize];

        // By position first, because which parameters those filled is what
        // makes a keyword of the same name a second value rather than a first.
        // The overflow only gets a vector of its own when there is some, which
        // for almost every call there is not.
        let count = positional_args.len();
        let mut extra: Vec<Object> = Vec::new();
        for (at, value) in positional_args.enumerate() {
            let value = value?;
            if at < positional {
                registers[at] = Some(value);
            } else {
                extra.push(value);
            }
        }

        // Then by name. Keyword failures are reported before a count that is
        // also wrong, which is the order CPython reports them in: `f(1, 2, y=3)`
        // on a one parameter function complains about `y`.
        let mut collected = Dict::new();
        let mut misplaced: Vec<Box<str>> = Vec::new();
        for (key, value) in by_name {
            let found = (shape.positional_only as usize..positional)
                .chain(keyword_slot..double_star_slot)
                .find(|slot| name(*slot) == &*key);
            let Some(slot) = found else {
                if shape.double_star {
                    // A `**kwargs` takes anything that did not match, including
                    // the name of a positional-only parameter, which is the one
                    // way that name can be passed at all.
                    collected.insert(ops::key(&Object::str(&*key), "keyword")?, value);
                    continue;
                }
                if (0..shape.positional_only as usize).any(|slot| name(slot) == &*key) {
                    misplaced.push(key);
                    continue;
                }
                return Err(self.wrong(&format!("got an unexpected keyword argument '{key}'")));
            };
            if registers[slot].is_some() {
                return Err(self.wrong(&format!("got multiple values for argument '{key}'")));
            }
            registers[slot] = Some(value);
        }
        if !misplaced.is_empty() {
            // Named in parameter order rather than the order they were passed,
            // so that `po(y=2, x=1)` reads the same as `po(x=1, y=2)`, and
            // comma joined inside one pair of quotes, which is the one list in
            // all of this that is not punctuated like the others.
            let names: Vec<&str> = (0..shape.positional_only as usize)
                .map(&name)
                .filter(|param| misplaced.iter().any(|key| &**key == *param))
                .collect();
            return Err(self.wrong(&format!(
                "got some positional-only arguments passed as keyword arguments: '{}'",
                names.join(", ")
            )));
        }

        if shape.star {
            registers[star_slot] = Some(Object::tuple(extra));
        } else if !extra.is_empty() {
            // The count is how many were passed by position, not how many were
            // left over, so a two argument function told five says five.
            let least = positional - self.defaults.len().min(positional);
            let allowed = if least == positional {
                format!("{positional} positional argument{}", plural(positional))
            } else {
                format!("from {least} to {positional} positional arguments")
            };
            // Keyword-only arguments that did arrive are spelled out, because
            // "takes 1 positional argument but 2 were given" with a `b=3` in
            // the call would be counting two different things with one number.
            // They are counted here rather than later because the defaults have
            // not been applied yet, so a filled keyword-only slot is one the
            // call filled.
            let by_keyword = (keyword_slot..double_star_slot)
                .filter(|slot| registers[*slot].is_some())
                .count();
            let given = if by_keyword == 0 {
                count.to_string()
            } else {
                format!(
                    "{count} positional argument{} (and {by_keyword} keyword-only argument{})",
                    plural(count),
                    plural(by_keyword)
                )
            };
            return Err(self.wrong(&format!(
                "takes {allowed} but {given} {} given",
                if count == 1 && by_keyword == 0 {
                    "was"
                } else {
                    "were"
                }
            )));
        }

        // Defaults fill from the right, which is the whole of the rule that a
        // parameter with a default cannot come before one without.
        let first_default = positional - self.defaults.len().min(positional);
        for (at, value) in self.defaults.iter().enumerate() {
            let slot = first_default + at;
            if slot < positional && registers[slot].is_none() {
                registers[slot] = Some(value.clone());
            }
        }
        let missing: Vec<&str> = (0..positional)
            .filter(|slot| registers[*slot].is_none())
            .map(&name)
            .collect();
        if !missing.is_empty() {
            return Err(self.wrong(&format!(
                "missing {} required positional argument{}: {}",
                missing.len(),
                plural(missing.len()),
                listed(&missing)
            )));
        }

        for (at, value) in self.kw_defaults.iter().enumerate() {
            let slot = keyword_slot + at;
            if slot < double_star_slot && registers[slot].is_none() {
                registers[slot].clone_from(value);
            }
        }
        let missing: Vec<&str> = (keyword_slot..double_star_slot)
            .filter(|slot| registers[*slot].is_none())
            .map(&name)
            .collect();
        if !missing.is_empty() {
            return Err(self.wrong(&format!(
                "missing {} required keyword-only argument{}: {}",
                missing.len(),
                plural(missing.len()),
                listed(&missing)
            )));
        }

        if shape.double_star {
            registers[double_star_slot] = Some(Object::dict(collected));
        }

        // Last, because a captured name can have the same spelling as a
        // parameter of an enclosing function and nothing above should be able
        // to reach these slots.
        for (reg, cell) in code.free.iter().zip(&self.captures) {
            if let Some(slot) = registers.get_mut(reg.0 as usize) {
                *slot = Some(cell.clone());
            }
        }
        Ok(registers)
    }

    /// A `TypeError` about calling this function, which every one of them is.
    fn wrong(&self, complaint: &str) -> Error {
        Error::type_error(format!("{}() {complaint}", self.code().name))
    }
}

/// The `s` on the end of "argument", which Python gets right even for zero.
fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// Names the way Python lists them: one plain, two joined by "and", and three or
/// more with commas and an Oxford comma before the last.
fn listed(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [only] => format!("'{only}'"),
        [first, second] => format!("'{first}' and '{second}'"),
        [rest @ .., last] => {
            let front: Vec<String> = rest.iter().map(|name| format!("'{name}'")).collect();
            format!("{}, and '{last}'", front.join(", "))
        }
    }
}

impl fmt::Debug for Function {
    /// The repr, because the code behind it is a listing rather than something
    /// to print inside a message.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.repr())
    }
}

impl Native for Function {
    fn type_name(&self) -> &'static str {
        "function"
    }

    fn repr(&self) -> String {
        // The address is what CPython shows and nothing may depend on it, which
        // is said once in `Native` and is worth remembering here: two functions
        // from the same `def` in a loop print differently on purpose.
        format!(
            "<function {} at {:#x}>",
            self.code().name,
            std::ptr::from_ref(self) as usize
        )
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
