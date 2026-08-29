//! The box two frames share when one of them closes over a name.
//!
//! A closure is not a copy of the values it captured. `def counter()` returning
//! an `inner` that does `nonlocal n; n += 1` has to see the same `n` the outer
//! frame has, and two functions defined in the same frame have to see each
//! other's writes. So a name shared that way lives in a cell, both frames hold
//! the same cell, and reading or writing the name goes through it.
//!
//! Which names those are is decided while the module is compiled, not while it
//! runs. Nothing pays for this except the names that need it, and a function
//! that captures nothing is exactly what it was before cells existed.
//!
//! A cell is never a value in Python. Nothing constructs one, no operator
//! accepts one, and the only four instructions that touch one are emitted by
//! the compiler in pairs it wrote itself. It is a [`Native`] because that is the
//! escape hatch for a value the object model has no shape for, which is the
//! honest description of it.

use std::any::Any;
use std::cell::RefCell;
use std::fmt;

use kohebi_core::{Native, Object};

/// A shared binding: one name, one place, however many frames.
#[derive(Default)]
pub struct Cell {
    /// Empty until something writes the name, and empty again after a `del`,
    /// which is what makes reading it a `NameError` rather than a `None`.
    value: RefCell<Option<Object>>,
}

impl Cell {
    /// A cell holding something, or nothing.
    #[must_use]
    pub fn new(value: Option<Object>) -> Self {
        Cell {
            value: RefCell::new(value),
        }
    }

    /// What is in it, or nothing if the name is not bound.
    #[must_use]
    pub fn get(&self) -> Option<Object> {
        self.value.borrow().clone()
    }

    /// Bind the name.
    pub fn set(&self, value: Object) {
        *self.value.borrow_mut() = Some(value);
    }

    /// Unbind it, which is what `del` on a shared name does.
    pub fn clear(&self) {
        *self.value.borrow_mut() = None;
    }
}

impl fmt::Debug for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.repr())
    }
}

impl Native for Cell {
    fn type_name(&self) -> &'static str {
        "cell"
    }

    fn repr(&self) -> String {
        match self.get() {
            Some(value) => format!("<cell at {:#x}: {}>", self.address(), value.type_name()),
            None => format!("<cell at {:#x}: empty>", self.address()),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Cell {
    fn address(&self) -> usize {
        std::ptr::from_ref(self) as usize
    }
}
