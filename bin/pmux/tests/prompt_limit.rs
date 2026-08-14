//! The one-megabyte prompt limit, which this tree states six times.
//!
//! MEASURED, live, at 2.1.227 on 2026-08-11: a `pmux ask` carrying
//! `MAX_PROMPT_BYTES + 1` bytes is refused by **this crate** with *"prompt
//! exceeds the 1048576-byte CLI limit"*, and never reaches
//! `pseudomux_service::driver_io::validate_prompt`, whose own refusal says
//! *"service limit"*. The two numbers are equal today and are tied by nothing:
//! `bin/pmux/src/cli.rs`, `bin/claude-p/src/main.rs` and
//! `crates/service/src/driver_io.rs` each declare `1024 * 1024` of their own,
//! and three test files declare a fourth, fifth and sixth copy to compare
//! against. Raise one and the client-side pre-check silently becomes the real
//! limit for every `pmux` caller while the daemon's message goes on describing
//! a bound nobody can reach.
//!
//! So the check here is not "the limit is 1 MiB" -- that is the literal, and a
//! test that restates a literal moves with it. It is that **every declaration
//! of this name in the tree states the same number**, with the set of
//! declarations read out of the tree rather than listed here. A seventh copy
//! added tomorrow is graded without this file being edited; a copy that
//! disagrees is named, with its path and line.
//!
//! It lives in `bin/pmux`'s tests rather than in `crates/service`'s because
//! `pseudomux-service` is one of the three packages `scripts/gate-a-mutants.sh`
//! re-runs for every mutant, and a test that reads the source tree is the same
//! answer 1,661 times. `bin/pmux` is not in that set.

#![cfg(unix)]

use std::path::{Path, PathBuf};

/// The workspace root, from this package's own manifest directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("bin/pmux is two levels under the workspace root")
        .to_path_buf()
}

/// Every `*.rs` under the first-party source directories, sorted.
///
/// `crates/` and `bin/` are where first-party Rust lives; `vendor/` is not
/// ours and `target/` is a build product. Both exclusions are by directory
/// rather than by pattern, so a new first-party crate under either root is
/// covered the day it lands.
fn first_party_rust(root: &Path) -> Vec<PathBuf> {
    fn walk(directory: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    for name in ["crates", "bin"] {
        walk(&root.join(name), &mut found);
    }
    found.sort();
    found
}

/// A `const NAME: TYPE = <expression>;` right-hand side, as a number.
///
/// Only a product of decimal integer literals is understood, which is the only
/// shape any declaration of this constant has ever had. Anything else returns
/// `None` and is reported as unreadable rather than silently skipped: a
/// declaration this function cannot parse is a declaration it cannot compare.
fn integer_product(expression: &str) -> Option<u64> {
    expression
        .split('*')
        .map(|factor| factor.trim().parse::<u64>().ok())
        .try_fold(1u64, |total, factor| Some(total * factor?))
}

#[derive(Debug)]
struct Declaration {
    where_: String,
    value: Option<u64>,
}

fn declarations_of(root: &Path, name: &str) -> Vec<Declaration> {
    let needle = format!("const {name}:");
    let mut found = Vec::new();
    for path in first_party_rust(root) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !text.contains(&needle) {
            continue;
        }
        for (index, line) in text.lines().enumerate() {
            let Some(offset) = line.find(&needle) else {
                continue;
            };
            let Some(body) = line[offset..].split_once('=') else {
                continue;
            };
            let relative = path.strip_prefix(root).unwrap_or(&path);
            found.push(Declaration {
                where_: format!("{}:{}", relative.display(), index + 1),
                value: integer_product(body.1.trim().trim_end_matches(';')),
            });
        }
    }
    found
}

/// Every statement of the prompt limit in this tree agrees with every other.
///
/// The failure this exists against is not a wrong number; it is two right
/// numbers that stop being one number. `bin/pmux` refuses an oversized prompt
/// before the daemon ever sees it, so the daemon's own limit is what a caller
/// meets only through some other client -- and the day these differ, which
/// limit applies depends on which binary the caller used.
#[test]
fn every_declaration_of_the_prompt_limit_states_the_same_number() {
    let root = workspace_root();
    let declarations = declarations_of(&root, "MAX_PROMPT_BYTES");

    // Vacuity, in the only two ways this scan can go quietly wrong: finding
    // nothing at all, and finding only the one it is standing next to.
    assert!(
        declarations.len() >= 2,
        "the scan found {} declaration(s) of MAX_PROMPT_BYTES, so it is grading \
         nothing: {declarations:?}",
        declarations.len()
    );
    assert!(
        declarations
            .iter()
            .any(|declaration| declaration.where_.starts_with("bin/pmux/src/cli.rs:")),
        "the scan did not find the declaration in bin/pmux/src/cli.rs, so it is not \
         reading the tree this test is in: {declarations:?}"
    );

    let unreadable: Vec<&str> = declarations
        .iter()
        .filter(|declaration| declaration.value.is_none())
        .map(|declaration| declaration.where_.as_str())
        .collect();
    assert!(
        unreadable.is_empty(),
        "these declarations of MAX_PROMPT_BYTES are not a product of integer \
         literals, so nothing here can compare them: {unreadable:?}"
    );

    let stated: Vec<(&str, u64)> = declarations
        .iter()
        .map(|declaration| (declaration.where_.as_str(), declaration.value.unwrap()))
        .collect();
    let (first_where, first_value) = stated[0];
    let disagreeing: Vec<&(&str, u64)> = stated
        .iter()
        .filter(|(_, value)| *value != first_value)
        .collect();
    assert!(
        disagreeing.is_empty(),
        "MAX_PROMPT_BYTES is {first_value} at {first_where} and something else \
         elsewhere: {disagreeing:?}. One prompt limit, stated in {} places, has to \
         be one number -- the client-side copy is what a `pmux` caller actually \
         meets, and the daemon's copy is what every other client meets.",
        stated.len()
    );
}
