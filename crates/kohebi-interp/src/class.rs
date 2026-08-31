//! Classes defined in Python, their instances, and the methods they hand out.
//!
//! A `class` statement runs a body in a frame of its own and keeps the
//! namespace that body filled in. That namespace is the class, which is why
//! everything a class knows is a name and a value and there is no separate
//! notion of a method: a `def` inside a class body binds a function into the
//! namespace exactly as it would anywhere else, and what makes it a method is
//! how it is looked up afterwards.
//!
//! ## Lookup
//!
//! An attribute of an instance is its own first and its class's second, and an
//! attribute of a class is its own first and its bases' second. Bases are a
//! chain rather than a graph here, because the lowering refuses a class with
//! more than one base until there is a C3 linearization to resolve them with.
//! When multiple inheritance arrives [`Class::lookup`] is the function that
//! grows an MRO, and nothing outside it has to change.
//!
//! ## Binding
//!
//! A function found on the class rather than on the instance comes back as a
//! [`Method`], which is the function and the instance together. That is where
//! `self` comes from: the method is called with one fewer argument than the
//! function takes, and the receiver fills the gap. A function found on the
//! *class* by way of the class itself is not bound, so `C.f(x)` and `x.f()` are
//! the same call, which is what Python says and is the whole of the difference
//! between the two lookups.
//!
//! ## What is missing
//!
//! Dunder methods, which is the large one. `__init__` runs because construction
//! has to put the arguments somewhere, but `__repr__`, `__eq__`, `__len__` and
//! the rest do not, because every one of them is user code called from inside an
//! operation that today has no way to call anything. So an instance prints the
//! address CPython prints for a class with no `__repr__`, compares by identity,
//! and is always true. Those are the right answers for a class that defines
//! none of them and the wrong ones for a class that does.
//!
//! There is also no metaclass, no `type()` of an instance as a value, no
//! `super`, no slots and no descriptor protocol beyond the one binding above.

use std::any::Any;
use std::cell::RefCell;
use std::fmt;

use kohebi_core::{Native, Object};
use rustc_hash::FxHashMap;

use crate::function::Function;

/// A namespace, which is a map from a name to whatever it is bound to.
pub type Names = FxHashMap<Box<str>, Object>;

/// A class defined by a `class` statement.
pub struct Class {
    name: Box<str>,
    /// `__qualname__`, which is what a repr prints. The same as the name for a
    /// class written at module level, which most are.
    qualname: Box<str>,
    /// The one base, or nothing for a class that named none.
    ///
    /// An [`Object`] rather than a `Class` because it is shared with whatever
    /// else refers to that class, and because the chain is walked by downcasting
    /// anyway. A base that is not a class cannot get here: the `class` statement
    /// that built this one evaluated it, and a non-class base is refused there.
    base: Option<Object>,
    /// What the body left behind, and what a class attribute is looked up in.
    ///
    /// Mutable, because `C.x = 1` after the fact is allowed and is how a good
    /// deal of Python is written.
    namespace: RefCell<Names>,
}

impl Class {
    /// The class a finished class body makes.
    #[must_use]
    pub fn new(name: Box<str>, qualname: Box<str>, base: Option<Object>, namespace: Names) -> Self {
        Class {
            name,
            qualname,
            base,
            namespace: RefCell::new(namespace),
        }
    }

    /// What the class is called, which is `__name__` and is also what an
    /// instance of it answers to `type_name`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the class is called with everything it is written inside in front,
    /// which is what a repr shows.
    #[must_use]
    pub fn qualname(&self) -> &str {
        &self.qualname
    }

    /// An attribute of the class or of a class behind it.
    ///
    /// Iterative rather than recursive so that a base chain a program built to
    /// be deep cannot take the Rust stack with it.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<Object> {
        let mut class = self;
        loop {
            if let Some(value) = class.namespace.borrow().get(name) {
                return Some(value.clone());
            }
            class = class.base.as_ref()?.downcast::<Class>()?;
        }
    }

    /// Bind an attribute on the class itself, leaving any base alone.
    pub fn set(&self, name: Box<str>, value: Object) {
        self.namespace.borrow_mut().insert(name, value);
    }

    /// Unbind one, giving back whether there was anything to unbind.
    ///
    /// Only the class's own namespace is touched, because `del C.x` on a name
    /// C inherited is an `AttributeError` rather than a way to hide the base's.
    pub fn delete(&self, name: &str) -> bool {
        self.namespace.borrow_mut().remove(name).is_some()
    }
}

impl fmt::Debug for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.repr())
    }
}

impl Native for Class {
    /// `type`, because that is the type of a class rather than the name of one.
    /// `type(C).__name__` is `type` and `C.__name__` is `C`, and this is the
    /// first of the two.
    fn type_name(&self) -> &str {
        "type"
    }

    fn repr(&self) -> String {
        // `__main__` because a script run directly is that module and there is
        // no other one yet. It becomes the real module name when imports do.
        format!("<class '__main__.{}'>", self.qualname)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An instance of a class defined in Python.
pub struct Instance {
    /// The class, held as an [`Object`] because that is what an attribute
    /// lookup that finds nothing has to walk into.
    class: Object,
    /// The instance's own attributes, which is `__dict__`.
    attributes: RefCell<Names>,
}

impl Instance {
    /// A fresh instance, before `__init__` has been given a chance at it.
    #[must_use]
    pub fn new(class: Object) -> Self {
        Instance {
            class,
            attributes: RefCell::new(Names::default()),
        }
    }

    /// The class this is an instance of.
    #[must_use]
    pub fn class(&self) -> &Object {
        &self.class
    }

    /// What is bound on the instance itself, ignoring the class.
    #[must_use]
    pub fn own(&self, name: &str) -> Option<Object> {
        self.attributes.borrow().get(name).cloned()
    }

    pub fn set(&self, name: Box<str>, value: Object) {
        self.attributes.borrow_mut().insert(name, value);
    }

    pub fn delete(&self, name: &str) -> bool {
        self.attributes.borrow_mut().remove(name).is_some()
    }
}

impl fmt::Debug for Instance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.repr())
    }
}

impl Native for Instance {
    fn type_name(&self) -> &str {
        // Nothing else in the runtime has a type name that is not a literal,
        // and this is the reason `Native::type_name` borrows.
        self.class.downcast::<Class>().map_or("object", Class::name)
    }

    fn repr(&self) -> String {
        // What CPython prints for a class with no `__repr__`, with the same
        // address in it and the same warning about depending on it. A class
        // that does define `__repr__` should be getting that instead, and does
        // not yet.
        let named = self
            .class
            .downcast::<Class>()
            .map_or("object", Class::qualname);
        format!(
            "<__main__.{named} object at {:#x}>",
            std::ptr::from_ref(self) as usize
        )
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A function looked up on an instance, with the instance it was looked up on.
///
/// Built by the lookup rather than stored anywhere, so `x.f` twice makes two of
/// these and they are not equal, which is what CPython does too.
pub struct Method {
    receiver: Object,
    /// The function, which is a [`Function`](crate::Function) held as an
    /// [`Object`] because the call machinery takes it back apart anyway.
    function: Object,
}

impl Method {
    #[must_use]
    pub fn new(receiver: Object, function: Object) -> Self {
        Method { receiver, function }
    }

    #[must_use]
    pub fn receiver(&self) -> &Object {
        &self.receiver
    }

    #[must_use]
    pub fn function(&self) -> &Object {
        &self.function
    }
}

impl fmt::Debug for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.repr())
    }
}

impl Native for Method {
    fn type_name(&self) -> &str {
        "method"
    }

    fn repr(&self) -> String {
        let named = self
            .function
            .downcast::<Function>()
            .map_or("?", |function| &function.code().qualname);
        format!("<bound method {named} of {}>", self.receiver.repr())
    }

    /// The same function bound to the same object.
    ///
    /// Both halves by identity, so `a.f == a.f` is true and `a.f == b.f` is
    /// false for two instances of one class, which is what CPython answers.
    /// Every lookup builds one of these, which is why the question comes up at
    /// all: `a.f is a.f` is false.
    fn equals(&self, other: &dyn Native) -> bool {
        other
            .as_any()
            .downcast_ref::<Method>()
            .is_some_and(|other| {
                self.receiver.is(&other.receiver) && self.function.is(&other.function)
            })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
