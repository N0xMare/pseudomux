# PR draft — rmux-server

**Title:** Gate `the_web_and_snapshot_renders_carry_no_title` behind `feature = "web"`

**Repo:** https://github.com/Helvesec/rmux
**Crate:** `rmux-server`, `--no-default-features` (i.e. without `web`)
**Affected:** 0.10.0 and `main` (`1f4571e7`). `src/handler_attach_tests/set_titles.rs` (md5
`6841d92f445f0e24d5686a399d0de4a9`) and `src/handler_attach.rs` (md5
`ed768880e172a082204fc197092ad15e`) are byte-identical across both, so the line numbers below hold
for each. Not affected: 0.9.0 and 0.9.1, where that test file does not exist and the same command
builds clean (measured below).
**Severity:** build break for any consumer that vendors `rmux-server` without the `web` feature and
runs its own `--all-targets` check. The shipped binary is unaffected; only test compilation fails.

Context: I ship `rmux-server` with `default-features = false` because I drive panes over the local
API and have no use for the web share surface, and my CI runs exactly
`cargo check --all-targets --no-default-features` — which is how this surfaced.

---

## Summary

`the_web_and_snapshot_renders_carry_no_title` calls two functions that only exist when the `web`
feature is on, but neither the test nor its module carries a matching gate. This PR adds the gate
to the single affected test.

Both definitions carry the gate — `src/handler_attach.rs:1271-1272` is `#[cfg(feature = "web")]`
above `pub(super) fn attach_target_for_web_session(`, and `:1379-1380` is the same pair above
`pub(super) fn attach_render_target_for_session_window(`. The callers,
`src/handler_attach_tests/set_titles.rs:575-593`, with no gate anywhere above them:

```rust
#[tokio::test]
async fn the_web_and_snapshot_renders_carry_no_title() {          // :576
    // ...
    // Exactly the two entry points handler_web.rs uses.
    let web_attach = crate::handler::attach_support::attach_target_for_web_session(   // :585
        &state, &alpha, 1, &OuterTerminalContext::default(), Path::new("rmux.sock"),
    )
    .expect("web attach target builds");
    let snapshot = crate::handler::attach_support::attach_render_target_for_session_window(  // :593
```

The module that declares it is ungated too: `src/handler_attach_tests.rs:888-889` is a bare
`#[path = "handler_attach_tests/set_titles.rs"] mod set_titles;`, identical on `main`.

The feature is named `web` and is in `default`. The 0.10.0 features table is
`default = ["web"]`, `web = [base64, getrandom, hkdf, httparse, rmux-web-crypto, serde, serde_json,
sha1, sha2, subtle, toml, zeroize]`, plus `fuzzing` and `perf-instrument`, both empty.
`handler_web.rs`, the only non-test caller of both functions, is itself gated — which is why the lib
builds fine and only the test target breaks.

The change:

```diff
--- a/crates/rmux-server/src/handler_attach_tests/set_titles.rs
+++ b/crates/rmux-server/src/handler_attach_tests/set_titles.rs
@@ -572,6 +572,7 @@
 /// The web and snapshot render paths share `render_prelude`, but a browser
 /// client advertises no terminal capabilities at all, so `set-titles on` must
 /// not push OSC 0 / OSC 7 into a snapshot that xterm.js will replay.
+#[cfg(feature = "web")]
 #[tokio::test]
 async fn the_web_and_snapshot_renders_carry_no_title() {
```

Gating the whole `mod set_titles;` would also work but is heavier than warranted: the other 13
tests in that file exercise the plain attach render path and compile fine without `web`.

## Reproduction

Pristine published crate, no patches, from `static.crates.io` and verified against the crates.io
API: sha256 `2802885d5aa5fb7ff927072103fcaecd458180065892879802df20420ca5204e`. Unpack it, then
`cargo check --locked --offline --all-targets --no-default-features`. macOS 15.7.7 arm64,
`rustc 1.97.1 (8bab26f4f 2026-07-14)`:

```
error[E0425]: cannot find function `attach_target_for_web_session` in module `crate::handler::attach_support`
    --> src/handler_attach_tests/set_titles.rs:585:54
     |
 585 |       let web_attach = crate::handler::attach_support::attach_target_for_web_session(
     |                                                        ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

error[E0425]: cannot find function `attach_render_target_for_session_window` in module `crate::handler::attach_support`
    --> src/handler_attach_tests/set_titles.rs:593:52
     |
 593 |       let snapshot = crate::handler::attach_support::attach_render_target_for_session_window(
     |                                                      ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^

error: could not compile `rmux-server` (lib test) due to 2 previous errors; 1 warning emitted
```

(Each error also carries a `:::` note pointing at the ungated neighbours `attach_target_for_session`
and `attach_render_target_for_session_with_prompt`; elided above.)

The same command on pristine 0.9.0 (sha256
`5b9e539353499018407a602ab5f916288bfa6b07d7a93764eeaf850effff0e8d`) and 0.9.1 (sha256
`33d3dc789e27ba3cc5c2355f78bd67e196674339c29bf47a2db1f830b233e0aa`) finishes with warnings only,
no errors.

## Verification

With that diff applied on an otherwise-pristine 0.10.0 tree:

* `cargo check --locked --offline --all-targets --no-default-features` — finishes clean, with the
  lib warning count unchanged at 9, so the gate introduces no new dead code. (The `unused import:
  dispatch_with_expected_window_identity` at `src/handler.rs:219` is pre-existing under
  `--no-default-features` and is not caused by this change.)
* `cargo check --locked --offline --all-targets` with default features — finishes clean.
* `cargo test --locked --offline --lib the_web_and_snapshot_renders_carry_no_title` with default
  features — `1 passed; 0 failed`, so the coverage is kept wherever `web` is on.
