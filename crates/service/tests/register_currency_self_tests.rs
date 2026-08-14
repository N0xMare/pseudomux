//! The survivor register's currency check, run by `cargo test` rather than by
//! a habit.
//!
//! `scripts/register_currency.py`, `scripts/mutation_register.py` and
//! `scripts/mutation_refilter.py` decide whether
//! `evidence/mutation-survivor-register.json` still describes the tree, and
//! criterion 1 of the Path B done-gate refuses on their answer. Their own tests
//! live in `scripts/tests` and are Python, which means the only thing that would
//! run them is somebody remembering to.
//!
//! This is that thing, and it is deliberately not a new Gate A cell. Gate A's
//! cell census is published, counted and covered by a receipt; adding a cell
//! makes every existing receipt short of one and the count wrong in two
//! documents. `cargo test --locked --workspace --all-targets --all-features` is
//! already a cell, so a test target under `crates/service/tests` is run by the
//! gate that exists, on the schedule that exists, with nothing to re-publish.
//!
//! `PYTHONDONTWRITEBYTECODE=1` is not decoration: `scripts/gate-a-residue.sh`
//! fails on a `__pycache__` in the source tree, and a test that leaves one
//! behind would redden the residue audit it is meant to sit beside.

use std::path::PathBuf;
use std::process::Command;

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("crates/service sits two levels below the workspace root")
        .to_path_buf()
}

/// Every test under `scripts/tests`, and the count they report is checked so
/// that a suite which stops discovering anything cannot pass as one that found
/// nothing to complain about.
#[test]
fn the_register_currency_suite_passes_and_discovers_its_tests() {
    let root = repository_root();
    let python = std::env::var("PMUX_DONE_PYTHON").unwrap_or_else(|_| "python3".into());
    let output = Command::new(&python)
        .args(["-m", "unittest", "discover", "-s", "scripts/tests"])
        .current_dir(&root)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()
        .expect("python3 must be on PATH to grade the register currency scripts");
    let report = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "scripts/tests refused:\n{report}{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let ran = report
        .lines()
        .find_map(|line| {
            line.strip_prefix("Ran ")
                .and_then(|rest| rest.split(' ').next())
        })
        .and_then(|count| count.parse::<usize>().ok())
        .unwrap_or(0);
    assert!(
        ran >= 20,
        "scripts/tests reported {ran} tests; a discovery that quietly stopped finding \
         them reports success over nothing\n{report}"
    );
    assert!(
        !root.join("scripts/tests/__pycache__").exists(),
        "the suite left a __pycache__ in the source tree, which the residue audit refuses"
    );
}
