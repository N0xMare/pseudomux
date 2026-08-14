//! One source scanner, shared by every derivation in this crate that has to
//! read its own code.
//!
//! # Why a scanner exists at all
//!
//! Two standing tests in this crate refuse to be given a hand-written list:
//! the differential entry-path test needs every route that can reach
//! admission, and
//! [`crate::driver_io`]'s rendering register needs every function that turns a
//! captured screen into a decision. A hand-written list is right on the day it
//! is written and silently narrows to nothing afterwards, which is the defect
//! both tests exist to prevent -- so both derive their input from these
//! functions and neither owns a copy.
//!
//! It was one copy inside `native.rs`'s own test module until the second
//! consumer arrived. A scanner stated twice is the same bug one level up: the
//! two copies would agree on the day they were split and would then be free to
//! disagree about what "production code" means, which is precisely the question
//! both derivations are asking.
//!
//! Test-only: this module is `#[cfg(test)]` at its declaration in `lib.rs`, and
//! [`declared_functions`] skips it for that reason -- checked, not assumed.

use std::path::{Path, PathBuf};

/// One `fn` this crate declares, as a source scan can see it.
#[cfg(unix)]
#[derive(Clone, Debug)]
pub(crate) struct DeclaredFunction {
    /// Path relative to `src/`, e.g. `native.rs` or `pool/mod.rs`.
    pub(crate) file: String,
    pub(crate) name: String,
    /// Whether the declaration carries any `pub`, i.e. whether a caller
    /// outside its own module can reach it.
    pub(crate) externally_visible: bool,
    pub(crate) body: String,
}

/// Every `fn` this crate declares outside a `mod tests` block.
///
/// Read from `CARGO_MANIFEST_DIR` AT TEST TIME rather than through a list
/// of `include_str!`s, because a list of `include_str!`s is a hand-written
/// list of files and a hand-written list is the defect this test exists to
/// prevent. A module added tomorrow is scanned without anyone remembering
/// to add a line here.
///
/// Bodies are cut by INDENTATION -- a free `fn` ends at a line that is
/// exactly `}`, an `impl` item at a line that is exactly four spaces and
/// `}` -- which is the rule
/// `run_once_is_the_only_start_that_forces_one_shot_retention` already
/// uses, and which `cargo fmt` guarantees and `gate_a/rust_fmt` asserts.
#[cfg(unix)]
pub(crate) fn declared_functions() -> Vec<DeclaredFunction> {
    fn walk(directory: &Path, found: &mut Vec<PathBuf>) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", directory.display()))
            .map(|entry| entry.expect("a readable directory entry").path())
            .collect();
        entries.sort();
        for entry in entries {
            if entry.is_dir() {
                walk(&entry, found);
            } else if entry.extension().and_then(|value| value.to_str()) == Some("rs") {
                found.push(entry);
            }
        }
    }

    // Every modifier that may precede `fn`. Stripping them is what makes
    // `pub(crate) async fn` and `async fn` one case; collecting whether any
    // of them began with `pub` is what makes visibility derived rather than
    // asked for a second time.
    const MODIFIERS: [&str; 8] = [
        "pub(crate) ",
        "pub(super) ",
        "pub(in crate) ",
        "pub ",
        "default ",
        "const ",
        "async ",
        "unsafe ",
    ];

    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&source_root, &mut files);
    // A floor, in the idiom `scripts/gate-a-residue.sh` uses for its own
    // derivation: a scan that silently stops finding files reports exactly
    // the same empty route set as one that found them and cleared them, so
    // it REFUSES instead.
    assert!(
        files.len() >= 20,
        "the source scan found only {} file(s) under {}; a derivation that \
         stopped matching must refuse, not report an empty route set",
        files.len(),
        source_root.display()
    );

    // The THIRD form of "not production code": a whole top-level module
    // declared `#[cfg(test)]` in `lib.rs`, which is what this file is. It is
    // derived from `lib.rs` rather than named here, and it is checked the same
    // way the `tests/` directory is -- a file skipped as test-only must be a
    // module `lib.rs` gates, so this cannot become a place to hide production
    // code from the derivation.
    let library = std::fs::read_to_string(source_root.join("lib.rs"))
        .expect("the crate root must be readable");
    let test_only_modules: Vec<String> = library
        .split("#[cfg(test)]\n")
        .skip(1)
        .filter_map(|after| after.lines().next())
        .filter_map(|line| line.strip_prefix("mod "))
        .filter_map(|line| line.strip_suffix(';'))
        .map(str::to_owned)
        .collect();
    assert!(
        !test_only_modules.is_empty(),
        "no `#[cfg(test)] mod <name>;` was found in {}; this scanner is one, so a \
         derivation that reads none has stopped matching and must refuse",
        source_root.join("lib.rs").display()
    );

    let mut declared = Vec::new();
    for path in files {
        if path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| {
                path.parent() == Some(source_root.as_path())
                    && test_only_modules.iter().any(|module| module == stem)
            })
        {
            continue;
        }
        // `mod tests` is not production code, in EITHER of its two forms.
        // The cut below removes the inline block; this removes the same
        // module's file-tree form, `src/<module>/tests/*.rs`, which is
        // where a test module too large to sit inline lives.
        //
        // MEASURED, not anticipated: `native/tests/seam.rs` builds
        // `StartSessionRequest` literals and calls the start funnel,
        // because that is what its subject is -- and this scan reported
        // two of its test functions as ROUTES INTO ADMISSION that
        // `ADMISSION_ROUTES` had to classify. A route table that has to
        // carry a row per test is a table nobody can read.
        //
        // The exclusion is checked rather than trusted: the module the
        // directory belongs to must be `#[cfg(test)]`-gated in the file
        // that declares it, so this cannot become a place to hide
        // production code from the derivation.
        if let Some(parent) = path.parent()
            && parent.file_name().is_some_and(|name| name == "tests")
        {
            // The file that DECLARES `mod tests`, which for `src/X/tests/`
            // is `src/X.rs`: the module is inline there and only its
            // children live in the directory.
            let owner = parent
                .parent()
                .expect("a tests directory sits inside a module directory")
                .with_extension("rs");
            let owner_text = std::fs::read_to_string(&owner).unwrap_or_else(|error| {
                panic!(
                    "{} holds a test module whose owning file {} must be readable: {error}",
                    path.display(),
                    owner.display()
                )
            });
            assert!(
                owner_text.contains("#[cfg(test)]\nmod tests {"),
                "{} was skipped as a test module, but {} does not declare `mod tests` under \
                 `#[cfg(test)]`",
                path.display(),
                owner.display()
            );
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
        let text = match text.find("\nmod tests {") {
            Some(cut) => &text[..cut],
            None => text.as_str(),
        };
        let file = path
            .strip_prefix(&source_root)
            .expect("every scanned file is under src/")
            .to_string_lossy()
            .into_owned();
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            let indent = line.len() - line.trim_start().len();
            if indent != 0 && indent != 4 {
                continue;
            }
            let mut rest = line.trim_start();
            let mut externally_visible = false;
            while let Some(modifier) = MODIFIERS
                .iter()
                .find(|modifier| rest.starts_with(**modifier))
            {
                externally_visible |= modifier.starts_with("pub");
                rest = &rest[modifier.len()..];
            }
            let Some(rest) = rest.strip_prefix("fn ") else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|value| value.is_alphanumeric() || *value == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            let closer = format!("{}}}", " ".repeat(indent));
            let body = lines[index + 1..]
                .iter()
                .take_while(|line| **line != closer)
                .copied()
                .collect::<Vec<&str>>()
                .join("\n");
            declared.push(DeclaredFunction {
                file: file.clone(),
                name,
                externally_visible,
                body,
            });
        }
    }
    declared
}

/// Whether `source` contains a call written `name(`.
///
/// Blind to the receiver, and deliberately: `self.f()`, `Type::f()` and
/// `f()` are one edge here. That OVER-collects, and over-collection is the
/// safe direction -- it can only add a route the classification table must
/// then account for. Under-collection is the failure this whole test exists
/// to prevent, so nothing is filtered out on suspicion.
#[cfg(unix)]
pub(crate) fn calls(source: &str, name: &str) -> bool {
    let mut rest = source;
    while let Some(at) = rest.find(name) {
        let preceding = rest[..at].chars().next_back();
        let following = &rest[at + name.len()..];
        let on_a_boundary = preceding.is_none_or(|value| !value.is_alphanumeric() && value != '_');
        if on_a_boundary && following.starts_with('(') {
            return true;
        }
        rest = &rest[at + name.len()..];
    }
    false
}

/// `source` with every whole-line comment removed.
///
/// `dispatch`'s arms are classified by what they CALL, and its `Ping` arm
/// names `Request::Diagnose` in prose. Without this the arm boundaries fall
/// in the wrong places and `Diagnose` is classified by somebody else's body.
#[cfg(unix)]
pub(crate) fn without_comment_lines(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<&str>>()
        .join("\n")
}
