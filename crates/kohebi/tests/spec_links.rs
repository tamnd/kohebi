//! Every `docs/spec/...md` mentioned in the tree has to exist.
//!
//! `kohebi-parse` pointed its `SPEC` constant and its module documentation at
//! `docs/spec/03-frontend.md` for three releases. There is no such file, and
//! nothing noticed, because a path in a doc comment is a string like any other.
//! A reference to a document that does not exist is worse than no reference,
//! since it sends a reader looking for a design that was never written down.
//!
//! This lives in the binary crate because it is about the repository rather
//! than about any one crate, and every crate is under the same root.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn every_referenced_spec_document_exists() {
    let root = repo_root();
    let mut missing = BTreeSet::new();
    let mut checked = 0;

    for file in sources(&root) {
        // This file talks about paths rather than using them, and its examples
        // are deliberately paths that do not exist.
        if file.ends_with("tests/spec_links.rs") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&file) else {
            continue; // Not UTF-8, so it is not a source file we wrote.
        };
        for reference in references(&text) {
            checked += 1;
            if !root.join(&reference).is_file() {
                missing.insert(format!(
                    "{} refers to {reference}",
                    file.strip_prefix(&root).unwrap_or(&file).display()
                ));
            }
        }
    }

    assert!(
        checked > 0,
        "found no spec references at all, so this test is not testing anything"
    );
    assert!(
        missing.is_empty(),
        "spec documents referenced but not present:\n{}",
        missing.into_iter().collect::<Vec<_>>().join("\n")
    );
}

/// The workspace root, which is two levels above this crate.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/kohebi is two levels below the root")
        .to_path_buf()
}

/// Files worth scanning: our Rust, our Markdown, and the manifests.
///
/// `target` and `experiments` are skipped. The first is build output and the
/// second has its own workspaces and its own README conventions.
fn sources(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in ["crates", "docs"] {
        walk(&root.join(dir), &mut found);
    }
    for entry in fs::read_dir(root)
        .expect("the repository root is readable")
        .flatten()
    {
        let path = entry.path();
        if path.is_file() && interesting(&path) {
            found.push(path);
        }
    }
    found.sort();
    found
}

fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            walk(&path, found);
        } else if interesting(&path) {
            found.push(path);
        }
    }
}

fn interesting(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("rs" | "md" | "toml")
    )
}

/// Every `docs/spec/<something>.md` in `text`, in the order they appear.
///
/// The scan stops at the first `.md` after the prefix, which is enough because
/// these paths never contain a space and never contain another `.md`. A hit
/// with anything quote-shaped in it is dropped rather than reported, since that
/// means the scan ran past the end of a real path into ordinary prose.
fn references(text: &str) -> Vec<String> {
    const PREFIX: &str = "docs/spec/";
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(PREFIX) {
        rest = &rest[start..];
        if let Some(end) = rest.find(".md") {
            let candidate = &rest[..end + ".md".len()];
            if !candidate.contains(['`', '"', ' ', '\n', '(', ')']) {
                out.push(candidate.to_owned());
            }
            rest = &rest[end..];
        } else {
            break;
        }
    }
    out
}

#[test]
fn the_scanner_finds_a_path_and_stops_at_the_end_of_it() {
    assert_eq!(
        references("see `docs/spec/00-README.md` for more"),
        ["docs/spec/00-README.md"]
    );
    assert_eq!(
        references("docs/spec/a.md and docs/spec/b.md"),
        ["docs/spec/a.md", "docs/spec/b.md"]
    );
    assert_eq!(references("nothing here"), Vec::<String>::new());
}

#[test]
fn a_run_on_line_of_prose_is_not_reported_as_a_path() {
    // Without the punctuation check this would claim a file called
    // `docs/spec/03 and also 04.md`, and the failure would be a puzzle.
    assert_eq!(
        references("docs/spec/03 and also 04.md"),
        Vec::<String>::new()
    );
}
