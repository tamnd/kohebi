//! Modules, and the import statement that goes looking for one.
//!
//! A module is a name and a namespace, and an import is the act of finding one
//! and binding something out of it. Nothing else about it is special: `sys.argv`
//! is an attribute lookup on an object, exactly as `x.f` is, and the object it
//! looks in is a map from a name to a value.
//!
//! ## What is built in
//!
//! Only `sys`, and only the part of it that is data. Every module here is
//! written in Rust and none is read off disk, so `import` resolves against a
//! table with one entry in it and says `No module named` to everything else,
//! which is what CPython says to a name it cannot find and is honest about what
//! this runtime has.
//!
//! Reading a module out of a file is the next piece and is why [`Modules`]
//! holds a dictionary rather than building a fresh module every time. That
//! dictionary is `sys.modules`, it is the same object the program can reach,
//! and a module is put in it before anything else happens to it, which is what
//! makes a cycle between two modules terminate rather than recurse.
//!
//! ## What `sys` says about the version
//!
//! `sys.version_info` reports 3.14, because that is the language this runtime
//! implements, and a program branching on the version wants to know what it may
//! use rather than what compiled the interpreter. `sys.version` names kohebi in
//! the same breath, so the two questions have two answers, which is the
//! arrangement every alternative implementation has settled on.

use std::any::Any;
use std::cell::RefCell;

use kohebi_core::{Dict, Error, Key, Kind, Native, Object, Result};

use crate::class::Names;

/// A module, which is a name and the namespace it holds.
pub struct Module {
    name: Box<str>,
    /// Where it was read from, or nothing for one written in Rust. This is what
    /// a repr prints and what `__file__` will be once modules come off disk.
    origin: Option<Box<str>>,
    namespace: RefCell<Names>,
}

impl Module {
    /// A module with a namespace already filled in.
    #[must_use]
    pub fn new(name: &str, origin: Option<&str>, namespace: Names) -> Self {
        Self {
            name: name.into(),
            origin: origin.map(Into::into),
            namespace: RefCell::new(namespace),
        }
    }

    /// Its name, which is `__name__`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What a name in it is bound to, if anything.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Object> {
        // `__name__` is the module rather than an entry in it, the same way a
        // class's name is, so it is answered before the namespace is read.
        if name == "__name__" {
            return Some(Object::str(&*self.name));
        }
        self.namespace.borrow().get(name).cloned()
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
        f.debug_struct("Module").field("name", &self.name).finish()
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
        }
    }

    /// The dictionary itself, which is what `sys.modules` is bound to.
    #[must_use]
    pub fn loaded(&self) -> &Object {
        &self.loaded
    }

    /// The module a dotted name refers to, importing it if this is the first
    /// time it has been asked for.
    ///
    /// `import a.b` binds `a`, so what comes back here is the module the whole
    /// name names and the caller is the one that decides which part of it to
    /// bind. There are no packages yet, so a dotted name never resolves and the
    /// complaint names the head of it, which is where CPython's search stops
    /// too.
    pub fn import(&self, name: &str) -> Result<Object> {
        if let Some(found) = self.get(name) {
            return Ok(found);
        }
        let built = match name {
            "sys" => self.sys(),
            _ => {
                let head = name.split('.').next().unwrap_or(name);
                return Err(Error::new(
                    Kind::ModuleNotFoundError,
                    format!("No module named '{head}'"),
                ));
            }
        };
        self.put(name, built.clone());
        Ok(built)
    }

    /// A module already imported, or nothing.
    ///
    /// Read out of the dictionary rather than out of a field, so a program that
    /// deletes an entry from `sys.modules` gets the import run again, which is
    /// how a test that wants a fresh module does it.
    fn get(&self, name: &str) -> Option<Object> {
        let Object::Dict(dict) = &self.loaded else {
            return None;
        };
        let found = dict.borrow().get(&key(name)).cloned();
        found
    }

    fn put(&self, name: &str, module: Object) {
        if let Object::Dict(dict) = &self.loaded {
            dict.borrow_mut().insert(key(name), module);
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

        bind("argv", Object::list(std::env::args().map(|arg| Object::str(arg.as_str())).collect()));
        bind("modules", self.loaded.clone());
        bind("path", Object::list(Vec::new()));
        bind("platform", Object::str(PLATFORM));
        bind("byteorder", Object::str(if cfg!(target_endian = "little") { "little" } else { "big" }));
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
        bind("builtin_module_names", Object::tuple(vec![Object::str("sys")]));

        Object::native(Module::new("sys", None, names))
    }
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

    #[test]
    fn a_module_imported_twice_is_the_same_object_both_times() {
        let modules = Modules::new();
        let first = modules.import("sys").expect("sys is built in");
        let second = modules.import("sys").expect("sys is built in");
        assert!(first.is(&second));
    }

    #[test]
    fn the_registry_is_the_dictionary_the_program_can_reach() {
        let modules = Modules::new();
        let sys = modules.import("sys").expect("sys is built in");
        let seen = sys
            .downcast::<Module>()
            .and_then(|module| module.get("modules"))
            .expect("sys.modules is bound");
        assert!(seen.is(modules.loaded()));
    }

    #[test]
    fn a_name_that_is_not_built_in_is_a_module_not_found_error() {
        let modules = Modules::new();
        let error = modules.import("nosuch").expect_err("nothing else is built in");
        assert_eq!(error.kind, Kind::ModuleNotFoundError);
        assert_eq!(error.message, "No module named 'nosuch'");
    }

    #[test]
    fn a_dotted_name_is_complained_about_by_its_head_because_that_is_where_the_search_stops() {
        let modules = Modules::new();
        let error = modules.import("a.b.c").expect_err("there are no packages");
        assert_eq!(error.message, "No module named 'a'");
    }

    #[test]
    fn a_modules_own_name_is_not_an_entry_in_it() {
        let module = Module::new("m", None, Names::default());
        module.set("__name__", Object::str("shadow"));
        let seen = module.get("__name__").expect("every module has a name");
        assert!(seen.equals(&Object::str("m")));
    }
}
