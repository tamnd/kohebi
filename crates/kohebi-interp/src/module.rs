//! Modules, and the import statement that goes looking for one.
//!
//! A module is a name and a namespace, and an import is the act of finding one
//! and binding something out of it. Nothing else about it is special: `sys.argv`
//! is an attribute lookup on an object, exactly as `x.f` is, and the object it
//! looks in is a map from a name to a value.
//!
//! ## Where a module comes from
//!
//! Two places. `sys` is built in, meaning it is written in Rust and handed over
//! rather than read, which is the arrangement CPython uses for it too. Anything
//! else is a `.py` file found on `sys.path`, read, compiled and run, and the
//! namespace its body left behind is the module.
//!
//! The search is single-file and top level. A directory with an `__init__.py` in
//! it is a package, and a package needs a `__path__` of its own for its
//! submodules to resolve against, so `import a.b` still refuses rather than
//! doing half of it. When the head of a dotted name is found and is a plain
//! file, the complaint says so in CPython's words.
//!
//! ## One namespace, not two
//!
//! A module's namespace and the globals its own code runs against are the same
//! storage, shared between the module object and the machine. That is what
//! makes `m.x` from outside and `x` from inside one binding, so a function in
//! `m` sees what somebody else assigned to `m.x` and a reader of `m.x` sees
//! what that function assigned to `x`.
//!
//! Keeping them apart would have been easier to write and would have been
//! wrong. Two copies of a namespace drift the moment either side writes, and
//! the drift shows up as a value that is stale rather than as an error, which
//! is the worst way for a runtime to be wrong.
//!
//! The machine lays the names out by index while a module is the one running,
//! which is why the shared thing is a cell it can take out of and put back
//! rather than a map it reads through. See [`crate::vm`].
//!
//! ## Why the registry is a dictionary
//!
//! Because the program can reach it. [`Modules`] holds `sys.modules` itself
//! rather than a private copy, so a program that reads that dictionary sees what
//! the runtime sees, and a program that deletes an entry gets the next import of
//! that name to run the file again.
//!
//! A module goes into it before its body runs, and that ordering is what makes a
//! cycle terminate: two modules importing each other means the second import
//! finds a module that exists and is half filled, which is exactly what CPython
//! gives you, rather than recursing until the stack runs out. A body that raises
//! is taken back out again, because a module that failed to initialise must not
//! be handed to the next import as though it had worked.
//!
//! ## What `sys` says about the version
//!
//! `sys.version_info` reports 3.14, because that is the language this runtime
//! implements, and a program branching on the version wants to know what it may
//! use rather than what compiled the interpreter. `sys.version` names kohebi in
//! the same breath, so the two questions have two answers, which is the
//! arrangement every alternative implementation has settled on.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use kohebi_core::{Dict, Error, Key, Kind, Native, Object, Result, Str};

use crate::class::Names;

/// A module, which is a name and the namespace it holds.
pub struct Module {
    name: Box<str>,
    /// Where it was read from, or nothing for one written in Rust. This is what
    /// a repr prints and what `__file__` is bound to.
    origin: Option<Box<str>>,
    /// Shared rather than owned, because a module's namespace and the globals
    /// its own code runs against are one thing. The machine holds the same cell
    /// and swaps its contents in and out of the slot layout as that module's
    /// code starts and stops running, so `m.x` and the `x` inside `m` are the
    /// same binding and neither can drift from the other.
    namespace: Namespace,
    /// Whether its body is still running.
    ///
    /// True only between a module going into `sys.modules` and its body
    /// finishing, which is the window a circular import lands in. A name it has
    /// not bound yet is missing for a reason worth saying out loud, so the
    /// complaint says which of the two it is.
    initializing: Cell<bool>,
}

/// A module's namespace, shared between the module object and the machine.
pub type Namespace = Rc<RefCell<Names>>;

impl Module {
    /// A module that is finished, which is one written in Rust.
    #[must_use]
    pub fn new(name: &str, origin: Option<&str>, namespace: Namespace) -> Self {
        Self {
            name: name.into(),
            origin: origin.map(Into::into),
            namespace,
            initializing: Cell::new(false),
        }
    }

    /// A module whose body has not run yet.
    #[must_use]
    pub fn loading(name: &str, origin: &str, namespace: Namespace) -> Self {
        let module = Module::new(name, Some(origin), namespace);
        module.initializing.set(true);
        module
    }

    /// Say its body has finished, so a name it has not got is simply absent.
    pub fn loaded(&self) {
        self.initializing.set(false);
    }

    /// Whether its body is still running.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.initializing.get()
    }

    /// Its name, which is `__name__`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What a name in it is bound to, if anything.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Object> {
        if let Some(value) = self.namespace.borrow().get(name) {
            return Some(value.clone());
        }
        // A module read off disk has `__name__` bound in it before its body
        // runs, so this is for the built-in ones, which have a name and no
        // body to bind it. Answered after the namespace rather than before, so
        // that a module which rebinds its own `__name__`, which is what a
        // script checking for `__main__` relies on, is believed.
        (name == "__name__").then(|| Object::str(&*self.name))
    }

    /// Where it was read from, or nothing for one written in Rust.
    #[must_use]
    pub fn origin(&self) -> Option<&str> {
        self.origin.as_deref()
    }

    /// The namespace itself, which is what the machine runs the module's own
    /// code against.
    #[must_use]
    pub fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    /// Bind a name in it, which is what `import x; x.y = 1` does.
    pub fn set(&self, name: &str, value: Object) {
        self.namespace.borrow_mut().insert(name.into(), value);
    }

    /// Unbind a name, saying whether there was one.
    pub fn remove(&self, name: &str) -> bool {
        self.namespace.borrow_mut().remove(name).is_some()
    }
}

impl std::fmt::Debug for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The name only. A module's namespace is every value a program bound at
        // module level, and printing that in a debug line is a page of output
        // and a cycle away from a hang, since a module can hold itself.
        f.debug_struct("Module")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl Native for Module {
    fn type_name(&self) -> &str {
        "module"
    }

    /// `<module 'sys' (built-in)>` for one written in Rust, and the path for one
    /// read off disk. CPython prints the same two shapes.
    fn repr(&self) -> String {
        match &self.origin {
            Some(origin) => format!("<module '{}' from '{origin}'>", self.name),
            None => format!("<module '{}' (built-in)>", self.name),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Every module that has been imported, which is `sys.modules`.
///
/// A dictionary rather than a map, because the program can reach it and read it
/// and put things in it, and a runtime that kept a private copy alongside would
/// have two answers to the same question.
#[derive(Debug)]
pub struct Modules {
    loaded: Object,
    /// The directories an import searches, which is `sys.path`.
    ///
    /// The same list the program can reach, for the same reason the registry is:
    /// a program that appends to `sys.path` and then imports expects the import
    /// to look where it just said. Held here as well as bound into `sys` so that
    /// the search works whether or not the program has imported `sys`.
    path: Object,
    /// The program's own arguments, which is `sys.argv`.
    ///
    /// Not `std::env::args`. Python's `sys.argv[0]` is the script and the rest
    /// are whatever came after it, so the runtime's own name and its flags are
    /// not in here and a program counting its arguments gets the number it
    /// expects. Empty until the caller running a script fills it in.
    argv: Object,
}

impl Default for Modules {
    fn default() -> Self {
        Self::new()
    }
}

impl Modules {
    /// An empty registry, which fills as the program imports things.
    #[must_use]
    pub fn new() -> Self {
        Self {
            loaded: Object::dict(Dict::new()),
            path: Object::list(Vec::new()),
            argv: Object::list(Vec::new()),
        }
    }

    /// Say what the program was called with.
    ///
    /// Written into the list rather than replacing it, so that a program which
    /// imported `sys` before this was called still sees it. In practice the
    /// caller does this first, but a rule that only holds in practice is one
    /// that breaks later.
    pub fn set_argv(&self, argv: &[String]) {
        if let Object::List(items) = &self.argv {
            let mut items = items.borrow_mut();
            items.clear();
            items.extend(argv.iter().map(|arg| Object::str(arg.as_str())));
        }
    }

    /// The dictionary itself, which is what `sys.modules` is bound to.
    #[must_use]
    pub fn loaded(&self) -> &Object {
        &self.loaded
    }

    /// Add a directory to the front of `sys.path`.
    ///
    /// The front, because `sys.path[0]` is the directory of the script being run
    /// and a module beside the script wins over one further away. That is the
    /// order CPython searches in and programs rely on it.
    pub fn add_path(&self, directory: &str) {
        if let Object::List(entries) = &self.path {
            entries.borrow_mut().insert(0, Object::str(directory));
        }
    }

    /// The file a top level module name would be read from, if there is one.
    ///
    /// Only `<entry>/<name>.py`. A directory with an `__init__.py` is a package
    /// and a package needs its own `__path__` for submodules to resolve against,
    /// so finding one here and importing it would be advertising something that
    /// does not work.
    fn find(&self, name: &str) -> Option<PathBuf> {
        let Object::List(entries) = &self.path else {
            return None;
        };
        // Collected first, because running the body of what this finds can
        // append to `sys.path` and the borrow must not still be open then.
        let directories: Vec<String> = entries
            .borrow()
            .iter()
            // A `sys.path` entry that is not a string is skipped rather than
            // complained about, which is what CPython does, and so is one
            // holding a lone surrogate, since no filesystem can name that.
            .filter_map(|entry| match entry {
                Object::Str(text) => match &**text {
                    Str::Utf8(directory) => Some(directory.to_string()),
                    Str::Wide(_) => None,
                },
                _ => None,
            })
            .collect();
        directories
            .into_iter()
            .map(|directory| Path::new(&directory).join(format!("{name}.py")))
            .find(|candidate| candidate.is_file())
    }

    /// Where a name's module is, without going and getting it.
    ///
    /// The split is deliberate. This file knows where a module comes from and
    /// the machine knows how to run one, so what comes back is either a module
    /// that needs nothing further or the path to a file that has to be compiled
    /// and executed, and only the caller can do the second.
    ///
    /// `import a.b` binds `a`, so the name asked about here is the whole dotted
    /// one and the caller decides which part of it to bind.
    pub(crate) fn resolve(&self, name: &str) -> Result<Found> {
        if let Some(found) = self.get(name) {
            return Ok(Found::Ready(found));
        }
        if name == "sys" {
            let built = self.sys();
            self.put(name, built.clone());
            return Ok(Found::Ready(built));
        }
        if let Some((head, _)) = name.split_once('.') {
            // A dotted name gets as far as its head and no further, because
            // there are no packages. Saying which of the two it is matters: the
            // head being a plain file is a different mistake from it being
            // absent, and CPython distinguishes them.
            return Err(if self.find(head).is_some() || head == "sys" {
                Error::new(
                    Kind::ModuleNotFoundError,
                    format!("No module named '{name}'; '{head}' is not a package"),
                )
            } else {
                missing(head)
            });
        }
        self.find(name)
            .map_or_else(|| Err(missing(name)), |path| Ok(Found::File(path)))
    }

    /// A module already imported, or nothing.
    ///
    /// Read out of the dictionary rather than out of a field, so a program that
    /// deletes an entry from `sys.modules` gets the import run again, which is
    /// how a test that wants a fresh module does it.
    pub(crate) fn get(&self, name: &str) -> Option<Object> {
        let Object::Dict(dict) = &self.loaded else {
            return None;
        };
        dict.borrow().get(&key(name)).cloned()
    }

    pub(crate) fn put(&self, name: &str, module: Object) {
        if let Object::Dict(dict) = &self.loaded {
            dict.borrow_mut().insert(key(name), module);
        }
    }

    /// Take a name back out, which is what happens when a module's body raised.
    ///
    /// A half initialised module must not be left where the next import will
    /// find it and hand it over as though it had worked.
    pub(crate) fn forget(&self, name: &str) {
        if let Object::Dict(dict) = &self.loaded {
            dict.borrow_mut().remove(&key(name));
        }
    }

    /// `sys`, or the data part of it.
    ///
    /// No functions and no streams. A stream is a type this runtime has not got
    /// and `sys.exit` is an exception that has to reach the top and set the
    /// process's status, so both are their own piece of work rather than an
    /// entry in a table.
    fn sys(&self) -> Object {
        let mut names = Names::default();
        let mut bind = |name: &str, value: Object| {
            names.insert(name.into(), value);
        };

        bind("argv", self.argv.clone());
        bind("modules", self.loaded.clone());
        bind("path", self.path.clone());
        bind("platform", Object::str(PLATFORM));
        bind(
            "byteorder",
            Object::str(if cfg!(target_endian = "little") {
                "little"
            } else {
                "big"
            }),
        );
        bind("maxsize", Object::int(i64::MAX));
        bind("maxunicode", Object::int(0x0010_FFFF));
        bind("version", Object::str(version().as_str()));
        bind(
            "version_info",
            Object::tuple(vec![
                Object::int(3),
                Object::int(14),
                Object::int(0),
                Object::str("final"),
                Object::int(0),
            ]),
        );
        bind(
            "executable",
            Object::str(
                std::env::current_exe()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default()
                    .as_str(),
            ),
        );
        bind(
            "builtin_module_names",
            Object::tuple(vec![Object::str("sys")]),
        );

        Object::native(Module::new("sys", None, Rc::new(RefCell::new(names))))
    }
}

/// What [`Modules::resolve`] found: either a module, or where to read one.
pub(crate) enum Found {
    /// Already imported or built in, so there is nothing left to do.
    Ready(Object),
    /// A file to read, compile and run.
    File(PathBuf),
}

/// The complaint for a name nothing answers to, in CPython's words.
fn missing(name: &str) -> Error {
    Error::new(
        Kind::ModuleNotFoundError,
        format!("No module named '{name}'"),
    )
}

/// A module name as a dictionary key, which cannot fail because a `str` always
/// has a hash.
fn key(name: &str) -> Key {
    Key::new(Object::str(name)).unwrap_or_else(|_| unreachable!("a str is hashable"))
}

/// What CPython calls this operating system, which is not what Rust calls it.
const PLATFORM: &str = if cfg!(target_os = "macos") {
    "darwin"
} else if cfg!(target_os = "windows") {
    "win32"
} else if cfg!(target_os = "linux") {
    "linux"
} else {
    "unknown"
};

/// `sys.version`, which names the language and then the thing implementing it.
///
/// CPython puts its own build in the brackets and every other implementation
/// puts its name there, so a program that prints this gets told both halves.
fn version() -> String {
    format!("3.14.0 (kohebi {})", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `resolve` found, for the cases in here where it is always a module.
    fn ready(modules: &Modules, name: &str) -> Result<Object> {
        match modules.resolve(name)? {
            Found::Ready(module) => Ok(module),
            Found::File(path) => panic!("nothing in these tests is on disk, but found {path:?}"),
        }
    }

    #[test]
    fn a_module_imported_twice_is_the_same_object_both_times() {
        let modules = Modules::new();
        let first = ready(&modules, "sys").expect("sys is built in");
        let second = ready(&modules, "sys").expect("sys is built in");
        assert!(first.is(&second));
    }

    #[test]
    fn the_registry_is_the_dictionary_the_program_can_reach() {
        let modules = Modules::new();
        let sys = ready(&modules, "sys").expect("sys is built in");
        let seen = sys
            .downcast::<Module>()
            .and_then(|module| module.get("modules"))
            .expect("sys.modules is bound");
        assert!(seen.is(modules.loaded()));
    }

    #[test]
    fn a_name_that_is_not_built_in_is_a_module_not_found_error() {
        let modules = Modules::new();
        let error = ready(&modules, "nosuch").expect_err("nothing else is built in");
        assert_eq!(error.kind, Kind::ModuleNotFoundError);
        assert_eq!(error.message, "No module named 'nosuch'");
    }

    #[test]
    fn a_dotted_name_is_complained_about_by_its_head_because_that_is_where_the_search_stops() {
        let modules = Modules::new();
        let error = ready(&modules, "a.b.c").expect_err("there are no packages");
        assert_eq!(error.message, "No module named 'a'");
    }

    /// A module written in Rust has no body to bind its own name, so the name
    /// it was made with answers for it.
    #[test]
    fn a_built_in_module_knows_its_name_without_anything_binding_it() {
        let module = Module::new("m", None, Namespace::default());
        let seen = module.get("__name__").expect("every module has a name");
        assert!(seen.equals(&Object::str("m")));
    }

    /// A module that rebinds its own `__name__` is believed, because that is
    /// what a script checking for `__main__` is relying on. The name it was made
    /// with is the fallback and not the answer.
    #[test]
    fn a_name_bound_in_the_namespace_wins_over_the_one_it_was_made_with() {
        let module = Module::new("m", None, Namespace::default());
        module.set("__name__", Object::str("__main__"));
        let seen = module.get("__name__").expect("it was just bound");
        assert!(seen.equals(&Object::str("__main__")));
    }
}
