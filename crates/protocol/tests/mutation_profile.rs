//! Is every assertion in this tree still a test under the profile it is built
//! with?
//!
//! `scripts/gate-a-mutants.sh` measures a mutation score with
//! `--profile mutants`, and the number only means what the gate says it means
//! if `debug-assertions` and `overflow-checks` are ON: with either off, every
//! `debug_assert!` and every arithmetic overflow in the tree stops being a
//! detector and the mutants they would have caught are scored as caught by
//! something else, or not caught at all.
//!
//! The gate USED to assert that by parsing `Cargo.toml` for
//! `[profile.mutants] = {inherits = "dev", debug = false}` and refusing any
//! other key. That predicate never mentioned either property. `mutants`
//! inherits `dev`, `Cargo.toml` declares no `[profile.dev]`, and both settings
//! therefore came from cargo defaults that nothing pinned: adding
//! `[profile.dev] debug-assertions = false` would have left the guard green
//! while the property it named was gone.
//!
//! So this OBSERVES the properties instead of reading a manifest. It is a test
//! rather than a script check because the only honest way to ask whether a
//! `debug_assert!` fires under a profile is to compile one under that profile
//! and fire it.
//!
//! It lives in `crates/protocol/tests/` for two reasons: `pseudomux-protocol`
//! is the cheapest package the mutation gate already builds under
//! `--profile mutants`, and `tests/` is outside the `crates/protocol/src/**`
//! mutation glob, so adding it leaves the enumerated mutant list -- and so the
//! score's denominator -- untouched.
//!
//! `PMUX_PROFILE_PROBE_REPORT` names a file this test writes the properties it
//! found live into, one per line. `assert_profile_properties_are_live` in
//! `scripts/gate-a-mutants.sh` compares that report against the set the gate
//! declares it is asserting, so a probe that quietly stops covering one of them
//! fails the gate instead of reporting agreement over a smaller set.

use std::hint::black_box;
use std::panic::{self, AssertUnwindSafe};

/// The properties observed, spelled exactly as cargo spells the profile keys.
/// `test_run_gate.py::test_the_mutation_gate_probes_every_profile_property_it_names`
/// reads these two literals out of this file and requires them to be the same
/// set the shell guard asserts.
const DEBUG_ASSERTIONS: &str = "debug-assertions";
const OVERFLOW_CHECKS: &str = "overflow-checks";

/// Run `body` with the panic hook silenced and report whether it panicked.
///
/// The hook is swapped because both probes are SUPPOSED to panic, and a gate
/// log full of "attempt to add with overflow" backtraces reads like a failure.
fn panics(body: impl FnOnce()) -> bool {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let outcome = panic::catch_unwind(AssertUnwindSafe(body));
    panic::set_hook(previous);
    outcome.is_err()
}

#[test]
fn every_assertion_in_the_tree_is_live_under_this_profile() {
    let mut live: Vec<&str> = Vec::new();

    // `debug_assert!` expands to `if cfg!(debug_assertions) { assert!(..) }`,
    // so with the property off the body never runs and nothing panics.
    if panics(|| debug_assert!(black_box(false), "mutation profile probe")) {
        live.push(DEBUG_ASSERTIONS);
    }
    // `black_box` keeps this out of const evaluation: a literal
    // `u8::MAX + 1` is the deny-by-default `arithmetic_overflow` lint and
    // would be a compile error rather than a measurement.
    if panics(|| {
        let _ = black_box(u8::MAX) + black_box(1u8);
    }) {
        live.push(OVERFLOW_CHECKS);
    }

    live.sort_unstable();
    if let Ok(report) = std::env::var("PMUX_PROFILE_PROBE_REPORT") {
        let body = live.iter().fold(String::new(), |mut text, name| {
            text.push_str(name);
            text.push('\n');
            text
        });
        std::fs::write(&report, body)
            .unwrap_or_else(|error| panic!("cannot write {report}: {error}"));
    }

    assert!(
        live.contains(&DEBUG_ASSERTIONS),
        "{DEBUG_ASSERTIONS} is off in this build: every debug_assert! in the \
         tree compiled to nothing, so a mutation score measured here counts \
         each of them as a test that does not exist"
    );
    assert!(
        live.contains(&OVERFLOW_CHECKS),
        "{OVERFLOW_CHECKS} is off in this build: arithmetic wraps instead of \
         panicking, so every mutant that only changes an operand is invisible"
    );
}
