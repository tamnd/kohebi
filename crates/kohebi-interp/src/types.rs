//! The type of a value, as a value.
//!
//! `type(x)` has to give back something, and until now there was nothing to
//! give. This is that something: one object per type, made once and handed out
//! every time, so `type(1) is type(2)` and `type(1) is int` are both true the
//! way they are in CPython.
//!
//! ## One object per name
//!
//! The machine keeps a table from a type's name to its type object. The names
//! a program can say are seeded into it from the builtins, so the `int` a
//! program writes and the `int` that `type(1)` finds are the same object. The
//! rest are filled in the first time something asks: `type(None)` builds a
//! `NoneType` and puts it there, and the second `type(None)` finds it. That is
//! what makes identity work for the types that have no name to be bound to.
//!
//! ## Which types can be called
//!
//! Three answers. Some are written, and calling one runs the constructor that
//! was already there under a different name. Some are real constructors this
//! runtime has not got to, and those say so, the same way an unwritten method
//! does. The rest are the types a program can only reach through `type(x)` at
//! all, like `generator` or `dict_keys`, and CPython refuses to construct those
//! too, in almost the same words.
//!
//! The two exceptions to that last rule are `NoneType` and `ellipsis`, which
//! CPython will construct and hand back the one value they have. Nothing here
//! does, which is a difference a program could see and which is not worth a
//! special case until something wants it.
//!
//! ## One name where CPython has two
//!
//! CPython gives a type a dotted name for its repr and a bare one for the
//! complaints it appears in: `repr(type(Path('.')))` is
//! `<class 'pathlib.PosixPath'>` and `len(Path('.'))` says `object of type
//! 'PosixPath'`. There is one name here, taken from [`Native::type_name`],
//! which is the bare one, so the repr of the handful of types with a module in
//! front of them is short by that much. Every type a program is likely to print
//! is built in and has no module, so this is visible only for `pathlib` today.
//!
//! ## What this is not
//!
//! It is not a namespace. A type object here holds a name and a way to make
//! one, and that is all, so `int.from_bytes` is still an `AttributeError` and
//! `class C(int)` is still refused. The method tables in [`crate::method`] are
//! the other half, and joining them is what turns these into the type objects
//! CPython has. Doing it in that order means `type` and `isinstance` work
//! first, which is what programs actually ask for.

use std::any::Any;
use std::fmt;

use kohebi_core::{Error, Native, Object, Result, exception};

use crate::builtin::Args;
use crate::class;
use crate::vm::{self, Vm};

/// What calling a type does, for the ones that can be called.
pub type Make = fn(&mut Vm, Args) -> Result<Object>;

/// What happens when a type is called.
#[derive(Clone, Copy)]
enum Made {
    /// Written, and this is it.
    By(Make),
    /// A real constructor that is not written here yet.
    Later,
    /// A type CPython will not make one of either.
    Never,
}

/// A builtin type, as a value.
pub struct Type {
    /// What the type is called, which is `__name__` and what the repr prints.
    ///
    /// Owned rather than borrowed because most of these are built from
    /// [`Native::type_name`], which borrows from the value it was asked about
    /// and so cannot outlive it.
    name: Box<str>,
    made: Made,
}

impl Type {
    /// A type that can be called, and this is what calling it does.
    #[must_use]
    pub fn made(name: &str, make: Make) -> Self {
        Type {
            name: name.into(),
            made: Made::By(make),
        }
    }

    /// A type whose constructor is real and is not written here yet.
    #[must_use]
    pub fn later(name: &str) -> Self {
        Type {
            name: name.into(),
            made: Made::Later,
        }
    }

    /// A type nothing can make one of, which is most of the ones that have no
    /// name in `builtins`.
    #[must_use]
    pub fn opaque(name: &str) -> Self {
        Type {
            name: name.into(),
            made: Made::Never,
        }
    }

    /// What the type is called.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Make one.
    ///
    /// # Errors
    ///
    /// Whatever the constructor raises, or a refusal for the types that have
    /// no constructor to run.
    pub fn call(&self, vm: &mut Vm, args: Args) -> Result<Object> {
        match self.made {
            Made::By(make) => make(vm, args),
            Made::Later => Err(vm::later(&format!("{}()", self.name))),
            Made::Never => Err(Error::type_error(format!(
                "cannot create '{}' instances",
                self.name
            ))),
        }
    }
}

impl fmt::Debug for Type {
    /// The repr, because the name is the only thing in here worth seeing and
    /// the derived one would print the address of the constructor.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.repr())
    }
}

impl Native for Type {
    /// `type`, because the type of a type is `type`. That is the fixed point
    /// the whole arrangement rests on and it is why `type(type) is type`.
    fn type_name(&self) -> &str {
        "type"
    }

    fn repr(&self) -> String {
        format!("<class '{}'>", self.name)
    }

    /// True, like every class. A type with nothing in it is still a type.
    fn truthy(&self) -> bool {
        true
    }

    /// The same type, by name.
    ///
    /// Identity would do for every type the machine hands out, because it hands
    /// out one per name. Comparing the name as well costs nothing and means a
    /// type built somewhere that did not go through the table still answers the
    /// question correctly.
    fn equals(&self, other: &dyn Native) -> bool {
        other
            .as_any()
            .downcast_ref::<Type>()
            .is_some_and(|other| self.name == other.name)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// What `object()` hands back, which is a value with nothing in it.
///
/// Nothing else in the runtime is this shape. Every other value is something:
/// a number, a container, a function, an instance of a class a program wrote.
/// This one is the bottom of the language, and what a program does with it is
/// exactly the two things it has, an identity and a truth. `sentinel =
/// object()` is the reason it exists, and that idiom needs no more than those.
///
/// It is not a `class::Instance` with an empty namespace, because an instance
/// points at the class it came from and there is no `object` class to point at:
/// `object` is a [`Type`], and a `Type` is a name and a constructor rather than
/// something an instance can be under. The two meet at the `of` function
/// instead, which answers `object` for this and so makes
/// `type(object()) is object` true.
pub struct Bare;

impl fmt::Debug for Bare {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.repr())
    }
}

impl Native for Bare {
    fn type_name(&self) -> &str {
        "object"
    }

    /// What CPython prints, address and all. There is nothing else to say
    /// about a value with nothing in it, which is why CPython prints an
    /// address here as well.
    fn repr(&self) -> String {
        format!(
            "<object object at {:#x}>",
            std::ptr::from_ref(self) as usize
        )
    }

    /// True. Only a value that says otherwise is false, and this one says
    /// nothing at all.
    fn truthy(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `object()`, which takes nothing at all.
///
/// Not even the arguments `object.__init__` would forward, because a subclass
/// is what makes those meaningful and a builtin type cannot be subclassed here
/// yet. CPython says the same thing for `object(1)` when nothing has been
/// subclassed either.
pub(crate) fn bare(_vm: &mut Vm, args: Args) -> Result<Object> {
    let (positional, named) = args.split();
    if positional.is_empty() && named.is_empty() {
        return Ok(Object::native(Bare));
    }
    Err(Error::type_error("object() takes no arguments"))
}

/// The type of a value, as a value.
pub(crate) fn of(vm: &mut Vm, value: &Object) -> Object {
    // An instance knows its own class, and that class is a value already, so
    // this is the one case the table has nothing to do with.
    if let Some(instance) = value.downcast::<class::Instance>() {
        return instance.class().clone();
    }
    vm.class_named(value.type_name())
}

/// Whether a value is a class, which is what `isinstance` and `issubclass`
/// need before they can ask anything about it.
///
/// Three kinds of thing are classes here: a builtin type, a builtin exception
/// class, and a class a program wrote. All three answer `type` to
/// [`Native::type_name`], and this asks them one at a time rather than asking
/// that, so a value that happens to call itself `type` cannot slip through.
#[must_use]
pub(crate) fn is_class(value: &Object) -> bool {
    value.downcast::<Type>().is_some()
        || value.downcast::<exception::Class>().is_some()
        || value.downcast::<class::Class>().is_some()
}

/// Whether one class is the other or is below it.
///
/// This is the whole of the inheritance graph the runtime has. It is small: the
/// exception tree, `bool` under `int`, a written class's chain of bases, and
/// `object` over all of it.
#[must_use]
pub(crate) fn derives(sub: &Object, sup: &Object) -> bool {
    if same(sub, sup) {
        return true;
    }
    // Every class in the language is below `object`, including `object`, which
    // the line above already answered.
    if named(sup, "object") {
        return true;
    }
    if let (Some(sub), Some(sup)) = (
        sub.downcast::<exception::Class>(),
        sup.downcast::<exception::Class>(),
    ) {
        return sub.kind().derives_from(sup.kind());
    }
    // The one builtin type whose base is not `object`. It is worth knowing
    // because `isinstance(True, int)` is true and a program that counts numbers
    // relies on it.
    if named(sub, "bool") && named(sup, "int") {
        return true;
    }
    ancestry(sub).any(|class| same(&class, sup))
}

/// The classes a written class is below, nearest first.
///
/// Iterative and by value, because the chain is shared and walking it by
/// reference would hold a borrow of every link at once.
fn ancestry(sub: &Object) -> impl Iterator<Item = Object> {
    let mut at = base_of(sub);
    std::iter::from_fn(move || {
        let class = at.take()?;
        at = base_of(&class);
        Some(class)
    })
}

/// The class a written class was declared under, when it named one.
fn base_of(class: &Object) -> Option<Object> {
    class.downcast::<class::Class>()?.base().cloned()
}

/// Whether two classes are the same class.
fn same(one: &Object, two: &Object) -> bool {
    if one.is(two) {
        return true;
    }
    // Two objects for one class should not happen, since each kind of class is
    // built once and shared. Comparing what they stand for anyway means a
    // second copy would still answer correctly rather than quietly not.
    if let (Some(one), Some(two)) = (one.downcast::<Type>(), two.downcast::<Type>()) {
        return one.name == two.name;
    }
    match (
        one.downcast::<exception::Class>(),
        two.downcast::<exception::Class>(),
    ) {
        (Some(one), Some(two)) => one.kind() == two.kind(),
        _ => false,
    }
}

/// Whether a class is the builtin type of this name.
fn named(value: &Object, name: &str) -> bool {
    value
        .downcast::<Type>()
        .is_some_and(|typed| typed.name() == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_prints_as_a_class_and_is_one() {
        let int = Object::native(Type::later("int"));
        assert_eq!(int.repr(), "<class 'int'>");
        assert_eq!(int.type_name(), "type");
        assert!(int.truthy());
        assert!(is_class(&int));
    }

    #[test]
    fn a_type_is_the_same_class_as_another_of_its_name() {
        let one = Object::native(Type::later("int"));
        let two = Object::native(Type::later("int"));
        let other = Object::native(Type::later("str"));
        assert!(!one.is(&two));
        assert!(same(&one, &two));
        assert!(!same(&one, &other));
    }

    #[test]
    fn everything_derives_from_object_and_bool_derives_from_int() {
        let object = Object::native(Type::later("object"));
        let int = Object::native(Type::later("int"));
        let truth = Object::native(Type::later("bool"));
        assert!(derives(&truth, &int));
        assert!(!derives(&int, &truth));
        assert!(derives(&int, &object));
        assert!(derives(&object, &object));
    }
}
