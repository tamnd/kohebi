//! `pathlib`, or the part of it that is path algebra.
//!
//! A path is text with a grammar. Everything a program does with one before it
//! touches a disk is reading that grammar: which part is the drive, where the
//! last separator is, what is left when you take the final name off. That half
//! is pure, it is most of what `pathlib` is for, and it is what this file is.
//!
//! ## Why the pieces are stored apart
//!
//! A [`Path`] holds a drive, a root and the names between the separators,
//! rather than holding the text and cutting it up on every question. Almost
//! every property is one of those three read straight off, `parent` is the
//! names with the last one dropped, and joining is one list on the end of
//! another. Keeping the text instead would mean parsing it again for each of
//! those, and the parsing is the only part with corners in it.
//!
//! The text is rebuilt when something asks for it, which is what makes
//! `Path('a//b/')` and `Path('a/b')` print the same. That is `pathlib`'s own
//! behaviour: it normalises separators and drops `.`, and it leaves `..` alone
//! because `a/../b` and `b` are different paths whenever `a` is a symbolic link.
//!
//! ## What the platform changes
//!
//! The separator, the repr, and whether there is a drive. A path is parsed with
//! [`std::path::Component`], which already knows that `C:\x` has a prefix on
//! Windows and that nothing does anywhere else, so the drive letters and UNC
//! shares come out right without this file spelling either of them out.
//!
//! The one thing that costs a line is POSIX's two leading slashes, which is a
//! root of its own and which Rust folds into a single one. `//a` keeps its
//! double slash here because `pathlib` keeps it, and three or more collapse.
//!
//! ## What is not here
//!
//! Anything that reads a directory or writes anything: `glob`, `iterdir`,
//! `mkdir`, `open`, `read_text`. Those are a file object and a directory walk,
//! which are their own pieces of work, and every one of their names is in the
//! `later` half of the method table so that asking for one says it is not
//! written yet rather than that `Path` has no such thing.
//!
//! `Path` is also a function that constructs rather than a class, because there
//! are no type objects for builtin types yet. It prints as a class and it
//! cannot be subclassed, which is the same gap `range` and `str` are in.

use std::any::Any;
use std::collections::HashSet;
use std::fs;
use std::path::{Component, MAIN_SEPARATOR, MAIN_SEPARATOR_STR, PathBuf};
use std::rc::Rc;

use kohebi_core::{Error, Kind, Native, Object, Result, Str, hash};

use crate::builtin::{Args, Builtin, Flavour};
use crate::class::Names;
use crate::module::Module;
use crate::vm::{self, Vm};

/// A filesystem path, taken apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path {
    /// The drive, which is `C:` or `\\server\share` on Windows and always
    /// empty everywhere else.
    drive: Box<str>,
    /// The separator that makes this absolute, or nothing when it is not.
    /// Usually one character; POSIX allows exactly two.
    root: Box<str>,
    /// The names between the separators, with `.` dropped and `..` kept.
    names: Vec<Box<str>>,
}

impl Path {
    /// The path a piece of text describes.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let native = std::path::Path::new(text);
        let mut drive = String::new();
        let mut root = String::new();
        let mut names = Vec::new();
        for part in native.components() {
            match part {
                Component::Prefix(prefix) => {
                    drive = prefix.as_os_str().to_string_lossy().into_owned();
                }
                Component::RootDir => MAIN_SEPARATOR_STR.clone_into(&mut root),
                // Dropped rather than kept, including a leading one, which is
                // where this differs from `std::path`: `Path('./a')` is `a`.
                Component::CurDir => {}
                Component::ParentDir => names.push("..".into()),
                Component::Normal(name) => names.push(name.to_string_lossy().into_owned().into()),
            }
        }
        // POSIX gives a path beginning with exactly two slashes a root of its
        // own, and `pathlib` passes that through. Rust does not, so it is put
        // back here.
        if cfg!(not(windows)) && text.starts_with("//") && !text.starts_with("///") {
            "//".clone_into(&mut root);
        }
        Path {
            drive: drive.into(),
            root: root.into(),
            names,
        }
    }

    /// The drive and the root together, which is what an absolute path starts
    /// with and what a relative one has none of.
    #[must_use]
    pub fn anchor(&self) -> String {
        format!("{}{}", self.drive, self.root)
    }

    /// The path as text, which is what `str` prints.
    ///
    /// A path with nothing in it is `.`, because that is the directory it
    /// refers to and because `pathlib` will not hand out an empty string.
    #[must_use]
    pub fn text(&self) -> String {
        let anchor = self.anchor();
        if self.names.is_empty() {
            return if anchor.is_empty() {
                ".".to_owned()
            } else {
                anchor
            };
        }
        let joined = self
            .names
            .iter()
            .map(|name| &**name)
            .collect::<Vec<_>>()
            .join(MAIN_SEPARATOR_STR);
        // No separator after a root, which already is one.
        if anchor.is_empty() || anchor.ends_with(MAIN_SEPARATOR) {
            format!("{anchor}{joined}")
        } else {
            format!("{anchor}{MAIN_SEPARATOR}{joined}")
        }
    }

    /// The same path with forward slashes, whatever this platform writes.
    #[must_use]
    pub fn as_posix(&self) -> String {
        self.text().replace('\\', "/")
    }

    /// Whether it starts at a root, so that nothing in front of it matters.
    ///
    /// Windows wants both halves: `C:x` names a drive and no root and is
    /// relative to wherever that drive's working directory is, and `\x` names a
    /// root and no drive and is relative to whichever drive you are on.
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        if cfg!(windows) {
            !self.drive.is_empty() && !self.root.is_empty()
        } else {
            !self.root.is_empty()
        }
    }

    /// The last name, or nothing for a path that is only a root.
    #[must_use]
    pub fn name(&self) -> &str {
        self.names.last().map_or("", |name| name)
    }

    /// Everything but the last name, or the path itself when there is none.
    #[must_use]
    pub fn parent(&self) -> Path {
        if self.names.is_empty() {
            return self.clone();
        }
        let mut parent = self.clone();
        parent.names.pop();
        parent
    }

    /// Where the extension starts in the final name, if it has one.
    ///
    /// A leading dot does not begin a suffix, which is why `.bashrc` has none.
    fn split_name(&self) -> Option<(&str, &str)> {
        let name = self.name();
        let at = name.rfind('.').filter(|at| *at > 0)?;
        Some(name.split_at(at))
    }

    /// The final extension, dot included, or nothing.
    #[must_use]
    pub fn suffix(&self) -> &str {
        self.split_name().map_or("", |(_, suffix)| suffix)
    }

    /// The final name without its extension.
    #[must_use]
    pub fn stem(&self) -> &str {
        self.split_name()
            .map_or_else(|| self.name(), |(stem, _)| stem)
    }

    /// Every extension, in the order they were written.
    #[must_use]
    pub fn suffixes(&self) -> Vec<String> {
        let name = self.name();
        // From the first dot that is not the leading one, so `.tar.gz` on a
        // hidden file gives two suffixes and `.bashrc` gives none.
        let Some(at) = name
            .get(1..)
            .and_then(|rest| rest.find('.'))
            .map(|at| at + 1)
        else {
            return Vec::new();
        };
        name[at..]
            .split('.')
            .skip(1)
            .map(|piece| format!(".{piece}"))
            .collect()
    }

    /// The anchor and then each name, which is what `parts` walks.
    #[must_use]
    pub fn parts(&self) -> Vec<String> {
        let anchor = self.anchor();
        let mut parts = Vec::with_capacity(self.names.len() + 1);
        if !anchor.is_empty() {
            parts.push(anchor);
        }
        parts.extend(self.names.iter().map(|name| (**name).to_owned()));
        parts
    }

    /// This path with another one on the end, or the other one when it is
    /// absolute, because a path with a root of its own ignores what is in front.
    #[must_use]
    pub fn join(&self, other: &Path) -> Path {
        if !other.root.is_empty() || !other.drive.is_empty() {
            return other.clone();
        }
        let mut joined = self.clone();
        joined.names.extend(other.names.iter().cloned());
        joined
    }

    /// This path with its final name replaced.
    ///
    /// # Errors
    ///
    /// A path with no final name has nothing to replace, which is a
    /// `ValueError` naming the path.
    pub fn with_name(&self, name: &str) -> Result<Path> {
        if self.names.is_empty() {
            return Err(self.empty_name());
        }
        let mut renamed = self.clone();
        renamed.names.pop();
        // Parsed rather than pushed, so that a name with a separator in it is
        // refused the way `pathlib` refuses it rather than making two names.
        let replacement = Path::parse(name);
        if !replacement.root.is_empty()
            || !replacement.drive.is_empty()
            || replacement.names.len() != 1
        {
            return Err(Error::new(
                Kind::ValueError,
                format!("Invalid name {}", Str::Utf8(name.into()).repr()),
            ));
        }
        renamed.names.extend(replacement.names);
        Ok(renamed)
    }

    /// This path with its extension replaced, or removed when the new one is
    /// empty.
    ///
    /// # Errors
    ///
    /// A suffix that is not empty and does not start with a dot, and a path
    /// with no final name to put one on.
    pub fn with_suffix(&self, suffix: &str) -> Result<Path> {
        if !suffix.is_empty() && (!suffix.starts_with('.') || suffix == ".") {
            return Err(Error::new(
                Kind::ValueError,
                format!("Invalid suffix {}", Str::Utf8(suffix.into()).repr()),
            ));
        }
        if self.names.is_empty() {
            return Err(self.empty_name());
        }
        self.with_name(&format!("{}{suffix}", self.stem()))
    }

    /// `PosixPath('/') has an empty name`, which is what `pathlib` says when
    /// there is no final name to work on.
    fn empty_name(&self) -> Error {
        Error::new(
            Kind::ValueError,
            format!("{} has an empty name", self.repr()),
        )
    }

    /// This path made absolute against the working directory, with nothing
    /// else touched.
    ///
    /// # Errors
    ///
    /// A working directory that cannot be read, which is what happens when the
    /// process is sitting in a directory somebody has since deleted.
    pub fn absolute(&self) -> Result<Path> {
        if self.is_absolute() {
            return Ok(self.clone());
        }
        let here = working_directory()?;
        Ok(here.join(self))
    }

    /// This path made absolute with every symbolic link in it followed and
    /// every `..` applied.
    ///
    /// Non-strict, which is `pathlib`'s default: a name along the way that does
    /// not exist is kept rather than complained about, so the result of
    /// resolving a path to a file you are about to create is where that file
    /// will be. A link that points at itself is left alone for the same reason.
    ///
    /// `..` is applied to what the links resolved to rather than to what was
    /// written, so `link/..` is the directory holding the link's target and not
    /// the directory holding the link. That is what the kernel does when it
    /// walks the same path, and it is what makes this different from tidying up
    /// the text.
    ///
    /// # Errors
    ///
    /// A working directory that cannot be read, for a relative path.
    pub fn resolve(&self) -> Result<Path> {
        let start = self.absolute()?;
        let mut walked = Path {
            drive: start.drive.clone(),
            root: start.root.clone(),
            names: Vec::new(),
        };
        // The links being expanded right now. A link reached while it is in
        // here is a cycle, and the way out is to stop expanding and keep the
        // name, which is what a non-strict resolve gives back.
        let mut expanding = HashSet::new();
        for name in &start.names {
            walked.step(name, &mut expanding);
        }
        Ok(walked)
    }

    /// Take one name of a path being resolved, following it if it is a link.
    fn step(&mut self, name: &str, expanding: &mut HashSet<String>) {
        if name == ".." {
            self.names.pop();
            return;
        }
        self.names.push(name.into());
        let here = self.text();
        if !expanding.insert(here.clone()) {
            // Already on the way through this one, so it points at itself
            // somewhere. Leave the name where it is.
            return;
        }
        if let Ok(target) = fs::read_link(&here) {
            let target = Path::parse(&target.to_string_lossy());
            // A relative link is relative to the directory holding the link,
            // which is where we were before the name went on.
            self.names.pop();
            if !target.root.is_empty() || !target.drive.is_empty() {
                self.drive = target.drive.clone();
                self.root = target.root.clone();
                self.names.clear();
            }
            for part in &target.names {
                self.step(part, expanding);
            }
        }
        expanding.remove(&here);
    }

    /// Whether anything is there, which is the one question the three
    /// filesystem predicates share.
    fn stat(&self) -> Option<fs::Metadata> {
        fs::metadata(self.text()).ok()
    }

    /// What `repr` prints, which names the flavour of path rather than the
    /// class, because that is what `pathlib` prints.
    fn repr(&self) -> String {
        format!(
            "{}({})",
            FLAVOUR,
            Str::Utf8(self.text().into_boxed_str()).repr()
        )
    }
}

impl Native for Path {
    fn type_name(&self) -> &str {
        FLAVOUR
    }

    fn repr(&self) -> String {
        Path::repr(self)
    }

    /// The text, which is what `str(p)` and `print(p)` want and is not the
    /// repr. This is one of the few native types where the two differ.
    fn display(&self) -> String {
        self.text()
    }

    /// Two paths with the same pieces, which is not the same question as two
    /// paths naming the same file: `a/../b` and `b` are not equal here and are
    /// not equal in `pathlib` either.
    fn equals(&self, other: &dyn Native) -> bool {
        other.as_any().downcast_ref::<Path>() == Some(self)
    }

    /// The text's hash, so that two equal paths land in the same slot.
    ///
    /// Without this a path would hash by address like every other native, and
    /// a dictionary keyed by paths would answer that a key it holds is not
    /// there, which is the sort of wrong that looks like a missing entry rather
    /// than like a bug.
    fn hash(&self) -> Option<i64> {
        hash::hash(&Object::str(self.text().as_str())).ok()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// What `pathlib` calls a path on this platform, which is what its repr says
/// and what an error message about its type says.
pub const FLAVOUR: &str = if cfg!(windows) {
    "WindowsPath"
} else {
    "PosixPath"
};

/// The `pathlib` module, which is `Path` and nothing else yet.
#[must_use]
pub fn module() -> Object {
    let mut names = Names::default();
    names.insert(
        "Path".into(),
        Object::native(Builtin::function("Path", construct, Flavour::Class)),
    );
    Object::native(Module::new(
        "pathlib",
        None,
        Rc::new(std::cell::RefCell::new(names)),
    ))
}

/// `Path(*segments)`, which joins them and takes the last absolute one as the
/// place to start.
// The same as in the builtin table: the signature a builtin body has is fixed,
// so one that only reads its arguments still takes them by value.
#[expect(clippy::needless_pass_by_value, reason = "the signature is fixed")]
fn construct(_vm: &mut Vm, args: Args) -> Result<Object> {
    args.rest("Path")?;
    let mut built = Path::parse("");
    for segment in args.positional() {
        built = built.join(&read(segment)?);
    }
    Ok(Object::native(built))
}

/// A path out of whatever a caller passed, in `pathlib`'s words for one that
/// cannot be.
pub(crate) fn read(value: &Object) -> Result<Path> {
    if let Some(already) = value.downcast::<Path>() {
        return Ok((*already).clone());
    }
    match value {
        Object::Str(text) => match &**text {
            Str::Utf8(text) => Ok(Path::parse(text)),
            // A lone surrogate is how CPython carries a filename the operating
            // system gave it that is not valid text. Nothing here can put one
            // back on the way out again, so refusing beats mangling it.
            Str::Wide(_) => Err(vm::later("a path holding a lone surrogate")),
        },
        other => Err(Error::type_error(format!(
            "argument should be a str or an os.PathLike object where \
             __fspath__ returns a str, not '{}'",
            other.type_name()
        ))),
    }
}

/// `a / b` when either side is a path, or nothing when neither is and this has
/// no business answering.
///
/// A path on one side and something that is not text on the other is left to
/// the ordinary operator, which words the complaint about both types the same
/// way `pathlib` does.
pub(crate) fn divide(left: &Object, right: &Object) -> Option<Object> {
    let (left, right) = (read(left).ok()?, read(right).ok()?);
    Some(Object::native(left.join(&right)))
}

/// The properties a path has, which are the ones that are not called.
///
/// `p.parent` is a path and `p.parent()` is a `TypeError`, so these cannot go
/// in the method table: what a lookup finds has to be the answer rather than
/// something to call.
pub(crate) fn property(path: &Path, name: &str) -> Option<Object> {
    let strings = |items: Vec<String>| items.iter().map(|s| Object::str(s.as_str())).collect();
    Some(match name {
        "parent" => Object::native(path.parent()),
        "name" => Object::str(path.name()),
        "stem" => Object::str(path.stem()),
        "suffix" => Object::str(path.suffix()),
        "suffixes" => Object::list(strings(path.suffixes())),
        "parts" => Object::tuple(strings(path.parts())),
        "anchor" => Object::str(path.anchor().as_str()),
        "drive" => Object::str(&*path.drive),
        "root" => Object::str(&*path.root),
        _ => return None,
    })
}

/// Where the process is, as a path.
fn working_directory() -> Result<Path> {
    let here: PathBuf =
        std::env::current_dir().map_err(|failed| Error::new(Kind::OSError, failed.to_string()))?;
    Ok(Path::parse(&here.to_string_lossy()))
}

/// Whether the thing at this path exists, is a file, or is a directory.
///
/// One function because the three differ only in what they ask the metadata,
/// and all three answer false rather than raising when there is nothing there,
/// which is what `pathlib` does without `strict=True`.
pub(crate) fn exists(path: &Path, want: Want) -> bool {
    path.stat().is_some_and(|found| match want {
        Want::Anything => true,
        Want::File => found.is_file(),
        Want::Directory => found.is_dir(),
    })
}

/// Which of the three questions [`exists`] was asked.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Want {
    /// `exists`.
    Anything,
    /// `is_file`.
    File,
    /// `is_dir`.
    Directory,
}

#[cfg(test)]
#[cfg(not(windows))]
mod tests {
    use super::*;

    /// The text of a parsed path, which is what almost every case here checks.
    fn text(written: &str) -> String {
        Path::parse(written).text()
    }

    #[test]
    fn separators_are_folded_and_a_lone_dot_is_dropped() {
        assert_eq!(text("a//b"), "a/b");
        assert_eq!(text("a/b/"), "a/b");
        assert_eq!(text("./a"), "a");
        assert_eq!(text("a/./b"), "a/b");
    }

    /// Because `a/../b` and `b` are different whenever `a` is a link, so
    /// tidying the text would change which file is meant.
    #[test]
    fn a_dotdot_is_left_where_it_was_written() {
        assert_eq!(text("a/../b"), "a/../b");
    }

    #[test]
    fn a_path_with_nothing_in_it_is_the_working_directory() {
        assert_eq!(text(""), ".");
        assert_eq!(text("."), ".");
    }

    #[test]
    fn a_root_survives_having_no_names_after_it() {
        assert_eq!(text("/"), "/");
        assert_eq!(text("//"), "//");
        assert_eq!(text("///"), "/");
    }

    #[test]
    fn the_parent_of_a_bare_name_is_here_and_the_parent_of_a_root_is_itself() {
        assert_eq!(Path::parse("a/b").parent().text(), "a");
        assert_eq!(Path::parse("a").parent().text(), ".");
        assert_eq!(Path::parse(".").parent().text(), ".");
        assert_eq!(Path::parse("/").parent().text(), "/");
        assert_eq!(Path::parse("/a").parent().text(), "/");
    }

    /// A leading dot is not the start of an extension, which is what keeps
    /// `.bashrc` a name rather than a suffix.
    #[test]
    fn a_hidden_file_has_a_stem_and_no_suffix() {
        assert_eq!(Path::parse(".bashrc").suffix(), "");
        assert_eq!(Path::parse(".bashrc").stem(), ".bashrc");
        assert_eq!(Path::parse(".tar.gz").suffixes(), vec![".gz"]);
    }

    #[test]
    fn only_the_last_extension_is_the_suffix_and_all_of_them_are_the_suffixes() {
        let path = Path::parse("a/b.tar.gz");
        assert_eq!(path.suffix(), ".gz");
        assert_eq!(path.stem(), "b.tar");
        assert_eq!(path.suffixes(), vec![".tar", ".gz"]);
    }

    #[test]
    fn an_absolute_segment_throws_away_everything_in_front_of_it() {
        let joined = Path::parse("a").join(&Path::parse("/b"));
        assert_eq!(joined.text(), "/b");
    }

    #[test]
    fn the_anchor_is_the_first_part_and_a_relative_path_has_none() {
        assert_eq!(Path::parse("/a/b").parts(), vec!["/", "a", "b"]);
        assert_eq!(Path::parse("a/b").parts(), vec!["a", "b"]);
        assert!(Path::parse(".").parts().is_empty());
    }

    #[test]
    fn a_suffix_that_is_not_one_is_refused_by_name() {
        let refused = Path::parse("a").with_suffix("txt").expect_err("no dot");
        assert_eq!(refused.message, "Invalid suffix 'txt'");
    }

    #[test]
    fn a_path_with_no_final_name_has_nothing_to_rename() {
        let refused = Path::parse("/").with_name("x").expect_err("no name");
        assert_eq!(refused.message, format!("{FLAVOUR}('/') has an empty name"));
    }

    #[test]
    fn replacing_a_suffix_puts_one_on_when_there_was_none_and_takes_one_off() {
        assert_eq!(
            Path::parse("a/b")
                .with_suffix(".txt")
                .expect("a name")
                .text(),
            "a/b.txt"
        );
        assert_eq!(
            Path::parse("a/b.py")
                .with_suffix("")
                .expect("a name")
                .text(),
            "a/b"
        );
    }

    #[test]
    fn two_paths_written_differently_are_equal_when_they_come_out_the_same() {
        assert_eq!(Path::parse("./a"), Path::parse("a"));
        assert_ne!(Path::parse("a/../b"), Path::parse("b"));
    }

    /// Equal values that hash differently would be filed in one slot and looked
    /// for in another.
    #[test]
    fn equal_paths_hash_equally() {
        let (one, same) = (Path::parse("./a"), Path::parse("a"));
        assert_eq!(Native::hash(&one), Native::hash(&same));
    }

    #[test]
    fn a_relative_path_resolves_against_the_working_directory() {
        let resolved = Path::parse("a/../b").resolve().expect("there is a cwd");
        assert!(resolved.is_absolute());
        assert_eq!(resolved.name(), "b");
    }

    /// Non-strict, so a path nothing has created yet still resolves, which is
    /// what makes this usable on a file about to be written.
    #[test]
    fn a_name_that_is_not_there_resolves_rather_than_complaining() {
        let resolved = Path::parse("/nowhere-at-all/deep/file")
            .resolve()
            .expect("nothing is read");
        assert_eq!(resolved.text(), "/nowhere-at-all/deep/file");
    }
}

#[cfg(test)]
#[cfg(windows)]
mod windows_tests {
    use super::*;

    /// A drive letter is a piece of its own, kept apart from the root, because
    /// `C:x` has a drive and no root and is relative to that drive's working
    /// directory rather than to the top of it.
    #[test]
    fn a_drive_is_not_the_root_and_a_path_needs_both_to_be_absolute() {
        let rooted = Path::parse(r"C:\a\b");
        assert_eq!(&*rooted.drive, "C:");
        assert_eq!(&*rooted.root, r"\");
        assert!(rooted.is_absolute());

        let relative = Path::parse("C:a");
        assert_eq!(&*relative.drive, "C:");
        assert_eq!(&*relative.root, "");
        assert!(!relative.is_absolute());

        let no_drive = Path::parse(r"\a");
        assert_eq!(&*no_drive.drive, "");
        assert!(!no_drive.is_absolute());
    }

    /// Both separators are read and one is written, which is what makes
    /// `Path('a/b')` and `Path(r'a\b')` the same path.
    #[test]
    fn a_forward_slash_is_read_and_a_backslash_is_written() {
        assert_eq!(Path::parse("a/b").text(), r"a\b");
        assert_eq!(Path::parse(r"a\b"), Path::parse("a/b"));
        assert_eq!(Path::parse(r"C:\a\b").text(), r"C:\a\b");
    }

    /// `as_posix` is the way back, and it is what a test that has to print a
    /// path on two platforms uses.
    #[test]
    fn as_posix_puts_the_forward_slashes_back() {
        assert_eq!(Path::parse(r"C:\a\b").as_posix(), "C:/a/b");
    }

    /// The anchor is the drive and the root together, and it is one part
    /// rather than two.
    #[test]
    fn the_anchor_is_the_first_part() {
        assert_eq!(Path::parse(r"C:\a\b").parts(), vec!["C:\\", "a", "b"]);
        assert_eq!(Path::parse("a/b").parts(), vec!["a", "b"]);
    }

    #[test]
    fn the_parent_of_a_drive_root_is_itself() {
        assert_eq!(Path::parse(r"C:\").parent().text(), r"C:\");
        assert_eq!(Path::parse(r"C:\a").parent().text(), r"C:\");
    }

    /// An absolute segment throws away what is in front of it here too, and so
    /// does one that names a drive.
    #[test]
    fn joining_an_absolute_segment_starts_again() {
        assert_eq!(Path::parse("a").join(&Path::parse(r"C:\b")).text(), r"C:\b");
        assert_eq!(
            Path::parse(r"C:\a").join(&Path::parse("b")).text(),
            r"C:\a\b"
        );
    }
}
