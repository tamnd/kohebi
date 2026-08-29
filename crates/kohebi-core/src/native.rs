//! Values this crate does not know the shape of.
//!
//! A Python program needs objects that are not data: the function `print` is
//! bound to, the iterator a `for` loop walks, the exception a `raise` throws.
//! None of them can be built here. There are no classes yet, so there is no way
//! to define a type from inside Python, and the ones the runtime needs depend
//! on the runtime rather than on the object model. `print` has to know where the
//! output goes and an iterator has to know how the interpreter steps it.
//!
//! Growing an [`Object`](crate::Object) variant for each of them would put every
//! one of those decisions in this crate, which is the wrong place for them and
//! is the sort of thing that is easy to add and hard to take back out. So the
//! layer above defines the type, implements [`Native`], and this crate asks it
//! the same handful of questions it asks any other value.
//!
//! Getting the concrete type back out is [`Native::as_any`] and a downcast.
//! That is the cost of the arrangement and it is paid only by the runtime, at
//! the two or three places that have to know whether the thing in a register is
//! the kind of object they can call or step.
//!
//! ## Identity, equality and hashing
//!
//! All three are the address, and none of them can be overridden. A function is
//! equal to itself and to nothing else, and that is what CPython says for one
//! too. When classes arrive and `__eq__` becomes user code the question moves
//! to the class rather than to this trait, so there is no point in an
//! overridable answer here that would have to be taken away again.

use std::any::Any;
use std::fmt;

/// A value whose type lives above this crate.
///
/// Implementors are runtime objects: builtin functions, iterators, exception
/// instances. They answer the questions any value has to answer and keep the
/// rest to themselves.
pub trait Native: fmt::Debug {
    /// What `type(x).__name__` says, which is what error messages need.
    fn type_name(&self) -> &'static str;

    /// What `repr` prints.
    ///
    /// CPython puts an address in most of these, as in
    /// `<built-in function print>` for one that has no address to show and
    /// `<list_iterator object at 0x102f9c130>` for one that does. An address is
    /// not reproducible between runs, so nothing may depend on the exact text.
    fn repr(&self) -> String;

    /// What `str` prints, which for a type with no `__str__` of its own is
    /// whatever `repr` prints. Almost every native type wants the default. An
    /// exception is the one that does not, because `str(e)` is the message and
    /// `repr(e)` is the call that would make it again.
    fn display(&self) -> String {
        self.repr()
    }

    /// Python's truth protocol, which for an object with no `__bool__` and no
    /// `__len__` is true. Almost every native type wants the default.
    fn truthy(&self) -> bool {
        true
    }

    /// The concrete value, for the runtime to downcast back to.
    fn as_any(&self) -> &dyn Any;
}
