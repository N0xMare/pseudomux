#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use sha2::{Digest, Sha256};

const PUBLISHED_IDENTITY: &str = include_str!("fixtures/rmux-server-0.9.0.sha256");
const CRATE_NAME: &str = "rmux-server";
const CRATE_VERSION: &str = "0.9.0";

/// The published file the patch's regressions live in.
///
/// Everything the lanes need about those regressions is DERIVED from this one
/// path: their names, from the two pmux-added spans of it named below, and the
/// module filter libtest matches them with, from the path itself. It is a
/// `const` so the derivation and the freeze cannot name different files —
/// `vendored_server_is_published_archive_plus_documented_attach_eof_patch`
/// pins these exact bytes against `patched_pane_io_tests_sha256`, which is what
/// makes a name read out of them a fact about the compiled crate rather than a
/// parse of an arbitrary file.
const PATCH_REGRESSION_SOURCE: &str = "src/pane_io/tests.rs";

/// The two pmux-added spans of [`PATCH_REGRESSION_SOURCE`], named ONCE.
///
/// `reconstruct_upstream_pane_io_tests` removes them to recover the published
/// file; `patch_regression_names` reads the same spans to learn what the patch
/// added. Both markers are upstream text, so neither entry here names a
/// regression — which is the point: this array is the reason no lane has to.
const PATCH_REGRESSION_BLOCKS: [(&[u8], &[u8], &str); 2] = [
    (
        b"\nasync fn run_preclosed_attach_input(",
        b"\n#[tokio::test]\nasync fn same_pid_replacement_publishes_while_validated_old_input_is_paused()",
        "orderly EOF regression block",
    ),
    (
        b"\nasync fn assert_preclosed_session_exit_during_input_pause_drains_final_output(",
        b"\n#[tokio::test]\nasync fn session_exit_before_input_validation_still_drains_final_output()",
        "preclosed close-publication regression block",
    ),
];

/// The document that publishes the derived set for every reader that cannot
/// parse Rust: the shell suite, the Python runner self-tests, and a human.
const PATCH_DOCUMENT: &str = "PMUX-PATCH.md";

/// The heading the published list follows in [`PATCH_DOCUMENT`].
const PATCH_DOCUMENT_LIST_HEADING: &str = "The exact regression names are:";

/// Where a patch-owned regression name may be spelled, as two BOUNDARIES.
///
/// The patched crate owns them, definition and publication both, so the first
/// boundary is [`vendor_root`] and not the two files inside it this used to
/// name. The second is the upstream reports: the repro `docs/upstream-issues/`
/// files against rmux IS one of these regressions, spelled in its source, in
/// the `cargo test` line a maintainer runs and in the measured failure output,
/// and `docs/rmux-upstream-state.md` records which vendored file it was copied
/// from. Neither is a lane. That half is not derived and cannot be, for the
/// reason [`REGRESSION_LANES`] gives — only the address tells a document that
/// quotes a name from a lane that restates one. MEASURED: `227063f` and
/// `41a25a0` each landed one, and this scan was red from the day each landed.
const UPSTREAM_REPORT_HOMES: [&str; 2] = ["docs/upstream-issues", "docs/rmux-upstream-state.md"];

/// Build output and tool caches, skipped by the scan. Everything else under
/// the workspace root is read and refused a restated regression name.
const SCAN_SKIPPED_DIRECTORIES: [&str; 8] = [
    ".git",
    ".context",
    ".pseudomux",
    ".ruff_cache",
    "__pycache__",
    "dist",
    "node_modules",
    "target",
];

/// The lanes that must RUN the derived set, and the file each one is written in.
///
/// Membership is not derived and cannot be: a lane that runs nothing looks
/// exactly like a file that is not a lane. What IS derived is everything
/// inside them — the filter each must carry and the gate name the three must
/// agree on — and the tree scan over the two home boundaries independently
/// catches a lane that goes back to naming regressions one at a time.
const REGRESSION_LANES: [&str; 3] = [
    "tools/gate-a-candidate/phase-manifest.json",
    "tools/linux-docker/suite.sh",
    "docs/testing.md",
];

/// The Linux projection of the candidate manifest: gate names only, no argv.
const LINUX_GATE_MANIFEST: &str = "tools/linux-docker/gate-a-manifest.json";

/// English cardinals by value. The patch document and `docs/testing.md` spell
/// the size of the regression set in prose; a spelled count is a claim, and a
/// claim is compared against the derived cardinality below.
const CARDINALS: [&str; 21] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
    "twenty",
];

/// Every prose claim about the SIZE of the patch-owned regression set, split
/// around the count word so the count is supplied by the derivation.
///
/// A floor, in the sense `test_run_gate.py`'s bug-class counter uses: each
/// phrasing must occur exactly once with the derived cardinality, so a
/// rephrasing that makes this scan stop finding a claim fails here rather than
/// reporting agreement over a smaller set of claims.
const REGRESSION_COUNT_CLAIMS: [(&str, &str, &str); 4] = [
    (
        "vendor/rmux-server/PMUX-PATCH.md",
        "its ",
        " regression tests",
    ),
    (
        "docs/testing.md",
        "run all ",
        " patch-owned EOF regressions",
    ),
    ("docs/testing.md", "file and ", " regressions differ"),
    (
        "docs/testing.md",
        "every one of the ",
        " patch-owned regressions",
    ),
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must resolve")
}

fn vendor_root() -> PathBuf {
    workspace_root().join("vendor/rmux-server")
}

fn identity() -> BTreeMap<&'static str, &'static str> {
    let mut identity = BTreeMap::new();
    for line in PUBLISHED_IDENTITY.lines() {
        let (key, value) = line.split_once('=').expect("identity row must contain =");
        assert!(
            !key.is_empty() && !value.is_empty(),
            "identity row is bounded"
        );
        assert!(
            identity.insert(key, value).is_none(),
            "duplicate identity key {key}"
        );
    }
    identity
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn collect_vendor_files(root: &Path, directory: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", directory.display()))
    {
        let entry = entry.expect("vendor directory entry must be readable");
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .unwrap_or_else(|error| panic!("could not stat {}: {error}", path.display()));
        assert!(
            !metadata.file_type().is_symlink(),
            "vendor tree contains symlink {}",
            path.display()
        );
        if metadata.is_dir() {
            collect_vendor_files(root, &path, files);
        } else {
            assert!(
                metadata.is_file(),
                "vendor tree contains special node {}",
                path.display()
            );
            assert_eq!(
                metadata.permissions().mode() & 0o111,
                0,
                "published source unexpectedly became executable: {}",
                path.display()
            );
            let relative = path
                .strip_prefix(root)
                .expect("vendor file must remain below root")
                .to_str()
                .expect("published vendor paths must be UTF-8")
                .replace(std::path::MAIN_SEPARATOR, "/");
            let bytes = fs::read(&path)
                .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
            assert!(
                files.insert(relative.clone(), bytes).is_none(),
                "duplicate vendor path {relative}"
            );
        }
    }
}

fn replace_once(bytes: &[u8], from: &[u8], to: &[u8], label: &str) -> Vec<u8> {
    let matches = bytes
        .windows(from.len())
        .enumerate()
        .filter_map(|(offset, candidate)| (candidate == from).then_some(offset))
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{label} must occur exactly once");
    let offset = matches[0];
    let mut replaced = Vec::with_capacity(bytes.len() - from.len() + to.len());
    replaced.extend_from_slice(&bytes[..offset]);
    replaced.extend_from_slice(to);
    replaced.extend_from_slice(&bytes[offset + from.len()..]);
    replaced
}

fn replace_exact_count(
    bytes: &[u8],
    from: &[u8],
    to: &[u8],
    expected: usize,
    label: &str,
) -> Vec<u8> {
    let matches = bytes
        .windows(from.len())
        .enumerate()
        .filter_map(|(offset, candidate)| (candidate == from).then_some(offset))
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        expected,
        "{label} must occur exactly {expected} times"
    );
    let mut replaced =
        Vec::with_capacity(bytes.len() - expected * from.len() + expected * to.len());
    let mut cursor = 0;
    for offset in matches {
        assert!(offset >= cursor, "{label} matches must not overlap");
        replaced.extend_from_slice(&bytes[cursor..offset]);
        replaced.extend_from_slice(to);
        cursor = offset + from.len();
    }
    replaced.extend_from_slice(&bytes[cursor..]);
    replaced
}

fn replace_between_once(
    bytes: &[u8],
    start: &[u8],
    end: &[u8],
    replacement: &[u8],
    label: &str,
) -> Vec<u8> {
    let starts = bytes
        .windows(start.len())
        .enumerate()
        .filter_map(|(offset, candidate)| (candidate == start).then_some(offset))
        .collect::<Vec<_>>();
    let ends = bytes
        .windows(end.len())
        .enumerate()
        .filter_map(|(offset, candidate)| (candidate == end).then_some(offset))
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1, "{label} start must occur exactly once");
    assert_eq!(ends.len(), 1, "{label} end must occur exactly once");
    assert!(starts[0] < ends[0], "{label} markers must be ordered");
    let mut replaced = Vec::with_capacity(bytes.len() - (ends[0] - starts[0]) + replacement.len());
    replaced.extend_from_slice(&bytes[..starts[0]]);
    replaced.extend_from_slice(replacement);
    replaced.extend_from_slice(&bytes[ends[0]..]);
    replaced
}

/// The bounds of the single span between `start` and `end`, on the same
/// exactly-once discipline every other reconstruction step uses.
fn span_once(bytes: &[u8], start: &[u8], end: &[u8], label: &str) -> (usize, usize) {
    let starts = bytes
        .windows(start.len())
        .enumerate()
        .filter_map(|(offset, candidate)| (candidate == start).then_some(offset))
        .collect::<Vec<_>>();
    let ends = bytes
        .windows(end.len())
        .enumerate()
        .filter_map(|(offset, candidate)| (candidate == end).then_some(offset))
        .collect::<Vec<_>>();
    assert_eq!(starts.len(), 1, "{label} start must occur exactly once");
    assert_eq!(ends.len(), 1, "{label} end must occur exactly once");
    assert!(starts[0] < ends[0], "{label} markers must be ordered");
    (starts[0], ends[0])
}

fn remove_between_once(bytes: &[u8], start: &[u8], end: &[u8], label: &str) -> Vec<u8> {
    let (from, to) = span_once(bytes, start, end, label);
    let mut replaced = Vec::with_capacity(bytes.len() - (to - from));
    replaced.extend_from_slice(&bytes[..from]);
    replaced.extend_from_slice(&bytes[to..]);
    replaced
}

/// The regression names the pmux patch ADDS to [`PATCH_REGRESSION_SOURCE`],
/// read out of the same two spans the upstream reconstruction removes.
///
/// A test the patch adds is exactly a test the reconstruction deletes, so this
/// is the same fact the `upstream_pane_io_tests_sha256` assertion already
/// proves, only named instead of hashed. Nothing here is a list: append a
/// fifteenth `#[tokio::test]` to either span and it appears in the return
/// value with no edit to this file.
fn patch_regression_names(patched: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    for (start, end, label) in PATCH_REGRESSION_BLOCKS {
        let (from, to) = span_once(patched, start, end, label);
        let block = std::str::from_utf8(&patched[from..to])
            .unwrap_or_else(|error| panic!("{label} must be UTF-8 text: {error}"));
        assert!(
            !block.contains("#[ignore"),
            "{label} carries an ignored test; the module filter would report \
             it skipped rather than run"
        );
        let mut attributed = false;
        for line in block.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("#[") {
                // `#[test]` and `#[tokio::test]`, and not `#[cfg(test)]`.
                attributed |= rest.trim_end_matches(']').ends_with("test");
                continue;
            }
            let signature = line
                .strip_prefix("async fn ")
                .or_else(|| line.strip_prefix("fn "));
            if let (true, Some(signature)) = (attributed, signature) {
                let name = signature
                    .split('(')
                    .next()
                    .expect("split always yields a first field");
                names.push(name.to_owned());
            }
            attributed = false;
        }
    }
    assert!(
        !names.is_empty(),
        "the patch spans declare no regression; the derivation is broken"
    );
    assert_eq!(
        names.iter().collect::<BTreeSet<_>>().len(),
        names.len(),
        "two patch regressions share a name: {names:?}"
    );
    names
}

/// The libtest filter that selects every derived regression, from the path
/// they are defined in rather than from a literal.
///
/// `src/pane_io/tests.rs` is `pane_io::tests`, so `pane_io::tests::` is a
/// prefix of every test in it. libtest matches a filter as a substring of the
/// full test name unless `--exact` is given, so this ONE argument runs all of
/// them — including one added after every lane was written, which is the whole
/// reason no lane spells a name any more.
fn patch_regression_module_filter() -> String {
    let module = PATCH_REGRESSION_SOURCE
        .strip_prefix("src/")
        .expect("the vendored library root is src/")
        .strip_suffix(".rs")
        .expect("the regression source is a Rust file")
        .replace('/', "::");
    format!("{module}::")
}

/// The cardinal a document must spell for a set of `count` things.
fn cardinal(count: usize) -> &'static str {
    let highest = CARDINALS.len() - 1;
    CARDINALS
        .get(count)
        .copied()
        .unwrap_or_else(|| panic!("no cardinal for {count}; CARDINALS stops at {highest}"))
}

fn read_workspace_text(relative: &str) -> String {
    let path = workspace_root().join(relative);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()))
}

/// Every file under the workspace root that is not build output or a cache.
fn scannable_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", directory.display()))
    {
        let path = entry
            .expect("workspace directory entry must be readable")
            .path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if SCAN_SKIPPED_DIRECTORIES.contains(&name) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)
            .unwrap_or_else(|error| panic!("could not stat {}: {error}", path.display()));
        if metadata.is_dir() {
            scannable_files(&path, files);
        } else if metadata.is_file() {
            files.push(path);
        }
    }
}

fn reconstruct_upstream_pane_io(patched: &[u8]) -> Vec<u8> {
    let with_upstream_pause_hooks = replace_between_once(
        patched,
        b"#[cfg(test)]\nstruct InstalledLiveAttachInputPause {",
        b"#[cfg(test)]\nasync fn pause_before_live_attach_input_validation",
        br#"#[cfg(test)]
static LIVE_ATTACH_INPUT_APPLY_PAUSE: std::sync::Mutex<
    Option<(
        crate::handler::attach_support::ActiveAttachIdentity,
        Arc<LiveAttachInputApplyPause>,
    )>,
> = std::sync::Mutex::new(None);

#[cfg(test)]
static LIVE_ATTACH_INPUT_VALIDATION_PAUSE: std::sync::Mutex<
    Option<(
        crate::handler::attach_support::ActiveAttachIdentity,
        Arc<LiveAttachInputApplyPause>,
    )>,
> = std::sync::Mutex::new(None);

#[cfg(test)]
fn install_live_attach_input_apply_pause(
    identity: crate::handler::attach_support::ActiveAttachIdentity,
) -> Arc<LiveAttachInputApplyPause> {
    let pause = Arc::new(LiveAttachInputApplyPause::default());
    *LIVE_ATTACH_INPUT_APPLY_PAUSE
        .lock()
        .expect("live attach input pause lock") = Some((identity, Arc::clone(&pause)));
    pause
}

#[cfg(test)]
fn install_live_attach_input_validation_pause(
    identity: crate::handler::attach_support::ActiveAttachIdentity,
) -> Arc<LiveAttachInputApplyPause> {
    let pause = Arc::new(LiveAttachInputApplyPause::default());
    *LIVE_ATTACH_INPUT_VALIDATION_PAUSE
        .lock()
        .expect("live attach input validation pause lock") = Some((identity, Arc::clone(&pause)));
    pause
}

"#,
        "patched EOF boundary pause hooks",
    );
    let with_upstream_pause_predicates = replace_exact_count(
        &with_upstream_pause_hooks,
        b".is_some_and(|installed| installed.identity == identity)",
        b".is_some_and(|(expected, _)| *expected == identity)",
        2,
        "patched live input pause identity predicate",
    );
    let with_upstream_validation_pause = replace_once(
        &with_upstream_pause_predicates,
        br#".expect("matching validation pause remains installed")
                    .pause"#,
        br#".expect("matching validation pause remains installed")
                    .1"#,
        "patched validation pause payload",
    );
    let with_upstream_apply_pause = replace_once(
        &with_upstream_validation_pause,
        br#".expect("matching pause remains installed")
                    .pause"#,
        br#".expect("matching pause remains installed")
                    .1"#,
        "patched apply pause payload",
    );
    let with_upstream_opportunistic_eof = replace_between_once(
        &with_upstream_apply_pause,
        b"            let mut attach_stream_closed = false;\n",
        b"            prime_persistent_overlay_barriers(\n",
        br#"            for _ in 0..MAX_IMMEDIATE_ATTACH_READS {
                match try_read_socket_bytes(&stream, &mut decoder)? {
                    TryAttachRead::Read => {}
                    TryAttachRead::Closed => {
                        log_attach_exit(
                            &live_input,
                            &current_target,
                            AttachExitReason::AttachStreamClosed,
                        );
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());
                    }
                    TryAttachRead::WouldBlock => break,
                }
            }
            process_attach_socket_messages(
                &mut decoder,
                &stream,
                &live_input,
                &closing,
                &mut current_target,
                &mut pending_input,
                &mut active_emit_cache,
                &mut locked,
                &mut pane_refresh,
                &mut pending_escape_flush,
                &mut last_client_input_at,
            )
            .await?;
"#,
        "patched opportunistic orderly EOF handling",
    );
    let with_upstream_selected_eof = replace_between_once(
        &with_upstream_opportunistic_eof,
        b"                result = read_socket_bytes(&stream, &mut decoder) => {\n",
        b"                _ = wait_for_refresh_deadline(pane_refresh.deadline()) => {\n",
        br#"                result = read_socket_bytes(&stream, &mut decoder) => {
                    if !result? {
                        log_attach_exit(
                            &live_input,
                            &current_target,
                            AttachExitReason::AttachStreamClosed,
                        );
                        let _ = emit_attach_stop(&stream, &current_target).await;
                        return Ok(());
                    }
                    process_attach_socket_messages(
                        &mut decoder,
                        &stream,
                        &live_input,
                        &closing,
                        &mut current_target,
                        &mut pending_input,
                        &mut active_emit_cache,
                        &mut locked,
                        &mut pane_refresh,
                        &mut pending_escape_flush,
                        &mut last_client_input_at,
                    )
                    .await?;
                }
"#,
        "patched selected-read orderly EOF handling",
    );
    let without_eof_helpers = remove_between_once(
        &with_upstream_selected_eof,
        br#"#[cfg(any(unix, windows))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachSocketReentry {
"#,
        br#"#[cfg(any(unix, windows))]
async fn sync_pending_escape_flush(
"#,
        "patched orderly EOF helper block",
    );
    let with_upstream_attach_processor = replace_between_once(
        &without_eof_helpers,
        br#"#[cfg(any(unix, windows))]
#[allow(clippy::too_many_arguments)]
async fn process_attach_socket_messages(
"#,
        br#"#[cfg(any(unix, windows))]
fn mark_attach_interactive_input(
"#,
        br#"#[cfg(any(unix, windows))]
#[allow(clippy::too_many_arguments)]
async fn process_attach_socket_messages(
    decoder: &mut AttachFrameDecoder,
    stream: &AttachTransport,
    live_input: &LiveAttachInputContext,
    closing: &AtomicBool,
    current_target: &mut types::OpenAttachTarget,
    pending_input: &mut Vec<u8>,
    active_emit_cache: &mut Option<(u64, rmux_proto::WindowTarget)>,
    locked: &mut bool,
    pane_refresh: &mut AttachRefreshScheduler,
    pending_escape_flush: &mut PendingEscapeFlush,
    last_client_input_at: &mut Option<Instant>,
) -> io::Result<()> {
    let forwarded_to_pane = match process_socket_messages(
        decoder,
        stream,
        live_input,
        Some(current_target),
        PendingAttachInputState::new(pending_input, pending_escape_flush),
        active_emit_cache,
        locked,
    )
    .await
    {
        Ok(forwarded_to_pane) => forwarded_to_pane,
        Err(_) if closing.load(Ordering::SeqCst) => {
            // A terminal attach control is queued before `closing` is
            // published. Input may already be between the queue poll and its
            // identity check when close removes the registration. Discard that
            // now-stale input and let the next loop iteration consume the
            // terminal control, which owns the finite output drain.
            PendingAttachInputState::new(pending_input, pending_escape_flush).clear();
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    if forwarded_to_pane {
        mark_attach_interactive_input(pane_refresh, last_client_input_at);
        if pane_refresh.is_pending() {
            pane_refresh.schedule_immediate();
        }
    }
    sync_pending_escape_flush(pending_escape_flush, live_input, pending_input).await;
    Ok(())
}

"#,
        "patched attach socket processing status",
    );
    let without_processing_status_support = remove_between_once(
        &with_upstream_attach_processor,
        br#"#[cfg(any(unix, windows))]
struct SocketMessageProcessing {
"#,
        br#"#[cfg(any(unix, windows))]
async fn process_socket_messages_with_status(
"#,
        "patched socket processing status support",
    );
    let with_upstream_processor_name = replace_once(
        &without_processing_status_support,
        b"async fn process_socket_messages_with_status(\n",
        b"async fn process_socket_messages(\n",
        "patched socket processor name",
    );
    let with_upstream_processor_return = replace_once(
        &with_upstream_processor_name,
        b") -> io::Result<SocketMessageProcessing> {\n",
        b") -> io::Result<bool> {\n",
        "patched socket processor return type",
    );
    let without_deferred_initialization = replace_once(
        &with_upstream_processor_return,
        b"    let mut reenter_outer_loop = false;\n",
        b"",
        "patched deferred-processing initialization",
    );
    let without_first_escape_deferral = replace_once(
        &without_deferred_initialization,
        br#"            if pending_escape_deadline_due(pending_escape_flush) {
                reenter_outer_loop = true;
                break 'messages;
            }
"#,
        br#"            if pending_escape_deadline_due(pending_escape_flush) {
                break 'messages;
            }
"#,
        "patched data-loop escape deferral",
    );
    let without_second_escape_deferral = replace_once(
        &without_first_escape_deferral,
        br#"        if pending_escape_deadline_due(pending_escape_flush) {
            reenter_outer_loop = true;
            break;
        }
"#,
        br#"        if pending_escape_deadline_due(pending_escape_flush) {
            break;
        }
"#,
        "patched message-loop escape deferral",
    );
    let without_unlock_deferral = replace_once(
        &without_second_escape_deferral,
        br#"                // Resuming terminal ownership is an inter-frame barrier. A
                // following binding may block indefinitely, so flush the
                // start sequence and render before decoding another frame.
                reenter_outer_loop = !decoder.is_empty();
                break 'messages;
"#,
        br#"                // Resuming terminal ownership is an inter-frame barrier. A
                // following binding may block indefinitely, so flush the
                // start sequence and render before decoding another frame.
                break 'messages;
"#,
        "patched unlock deferral",
    );
    replace_once(
        &without_unlock_deferral,
        br#"    Ok(SocketMessageProcessing {
        forwarded_to_pane,
        reenter_outer_loop,
    })
"#,
        b"    Ok(forwarded_to_pane)\n",
        "patched socket processing outcome",
    )
}

fn reconstruct_upstream_pane_io_tests(patched: &[u8]) -> Vec<u8> {
    let with_upstream_imports = replace_between_once(
        patched,
        b"use super::{\n",
        b"};\nuse crate::daemon::ShutdownHandle;\n",
        br#"use super::{
    clear_close_pane_output_after_refresh_if_target_changed, consume_predicted_echo,
    finish_pending_attach_exit_with_batch, forward_attach, install_live_attach_input_apply_pause,
    install_live_attach_input_validation_pause, is_predictable_local_echo, pane_output_channel,
    pane_output_channel_with_limits, pending_attach_exit_output_batch,
    predictable_local_echo_prefix_len, process_attach_data_payload, process_socket_messages,
    should_emit_overlay, sync_pending_escape_flush_with_escape_time, AttachControl,
    AttachControlSender, AttachTarget, LiveAttachInputContext, OverlayFrame, PredictedEcho,
"#,
        "patched pane I/O regression imports",
    );
    let mut without_regressions = with_upstream_imports;
    for (start, end, label) in PATCH_REGRESSION_BLOCKS {
        without_regressions = remove_between_once(&without_regressions, start, end, label);
    }
    without_regressions
}

fn canonical_tree_sha256(files: &BTreeMap<String, Vec<u8>>) -> String {
    let mut digest = Sha256::new();
    for (path, bytes) in files {
        let path = path.as_bytes();
        digest.update(
            u64::try_from(path.len())
                .expect("path length fits")
                .to_be_bytes(),
        );
        digest.update(path);
        digest.update(
            u64::try_from(bytes.len())
                .expect("file length fits")
                .to_be_bytes(),
        );
        digest.update(bytes);
    }
    format!("{:x}", digest.finalize())
}

#[test]
fn vendored_server_is_published_archive_plus_documented_attach_eof_patch() {
    let identity = identity();
    assert_eq!(identity.get("schema"), Some(&"1"));
    assert_eq!(identity.get("crate"), Some(&CRATE_NAME));
    assert_eq!(identity.get("version"), Some(&CRATE_VERSION));
    assert_eq!(identity.len(), 11, "published identity field set changed");

    let root = vendor_root();
    let mut actual = BTreeMap::new();
    collect_vendor_files(&root, &root, &mut actual);
    let patch_document = actual
        .remove(PATCH_DOCUMENT)
        .expect("the local patch document is required");
    let expected_count = identity["file_count"]
        .parse::<usize>()
        .expect("published file count must be an integer");
    assert_eq!(actual.len(), expected_count);

    let pane_io = actual
        .get_mut("src/pane_io.rs")
        .expect("published production attach source is required");
    assert_eq!(sha256(pane_io), identity["patched_pane_io_sha256"]);
    *pane_io = reconstruct_upstream_pane_io(pane_io);
    assert_eq!(sha256(pane_io), identity["upstream_pane_io_sha256"]);

    let pane_io_tests = actual
        .get_mut(PATCH_REGRESSION_SOURCE)
        .expect("published attach test source is required");
    assert_eq!(
        sha256(pane_io_tests),
        identity["patched_pane_io_tests_sha256"]
    );
    let regressions = patch_regression_names(pane_io_tests);
    *pane_io_tests = reconstruct_upstream_pane_io_tests(pane_io_tests);
    assert_eq!(
        sha256(pane_io_tests),
        identity["upstream_pane_io_tests_sha256"]
    );
    assert_eq!(
        canonical_tree_sha256(&actual),
        identity["published_tree_sha256"],
        "reconstructed tree must equal the complete published crate"
    );

    let vcs: Value = serde_json::from_slice(
        actual
            .get(".cargo_vcs_info.json")
            .expect("VCS identity is required"),
    )
    .expect("VCS identity must be valid JSON");
    assert_eq!(vcs["git"]["sha1"].as_str(), Some(identity["vcs_sha1"]));

    let patch_document =
        String::from_utf8(patch_document).expect("patch document must be UTF-8 text");
    // The regression names are NOT restated here. They are the ones just read
    // out of the frozen patched bytes, so this loop grows by itself when the
    // patch does -- which is the difference between a document that is checked
    // and a document that is agreed with.
    let required = [
        identity["archive_sha256"],
        identity["vcs_sha1"],
        identity["published_tree_sha256"],
        "crates/rmux/tests/vendor_server_patch.rs",
        "real_attach_half_close_delivers_the_final_complete_frame_exactly_once",
        "uses_default_features = false",
        "exactly empty feature set",
        "UnexpectedEof",
    ];
    for required in required
        .iter()
        .copied()
        .chain(regressions.iter().map(String::as_str))
    {
        assert!(
            patch_document.contains(required),
            "patch document is missing {required}"
        );
    }
}

/// The document publishes exactly the derived set, and says how many.
///
/// `PMUX-PATCH.md` is the one file other than the source that may spell these
/// names, and it exists because nothing else in the tree can parse Rust: the
/// shell suite and the Linux runner self-tests read it. That makes its list a
/// SECOND spelling of the patch, so it is compared here against the first --
/// element for element, not merely for containment, which is all
/// `vendored_server_is_published_archive_plus_documented_attach_eof_patch`
/// could ever do on its own. A name deleted from the document used to be
/// invisible; now it is this assertion.
#[test]
fn the_patch_document_publishes_exactly_the_regressions_the_patch_adds() {
    let patched = fs::read(vendor_root().join(PATCH_REGRESSION_SOURCE))
        .expect("published attach test source is required");
    let derived = patch_regression_names(&patched)
        .into_iter()
        .collect::<BTreeSet<_>>();

    let document = fs::read_to_string(vendor_root().join(PATCH_DOCUMENT))
        .expect("the patch document is required");
    let (_, listed) = document
        .split_once(PATCH_DOCUMENT_LIST_HEADING)
        .unwrap_or_else(|| {
            panic!(
                "{PATCH_DOCUMENT} no longer heads its list with the sentence \
             {PATCH_DOCUMENT_LIST_HEADING:?}; the derivation reads nothing"
            )
        });
    let published = listed
        .lines()
        .skip_while(|line| line.trim().is_empty())
        .map_while(|line| line.trim().strip_prefix("- `"))
        .map(|line| {
            line.split('`')
                .next()
                .expect("split always yields a first field")
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        published.iter().collect::<BTreeSet<_>>().len(),
        published.len(),
        "the patch document lists a regression twice: {published:?}"
    );
    assert_eq!(
        published.into_iter().collect::<BTreeSet<_>>(),
        derived,
        "the patch document's list and the patch's own tests disagree"
    );

    let spelled = cardinal(derived.len());
    for (relative, before, after) in REGRESSION_COUNT_CLAIMS {
        let text = read_workspace_text(relative);
        assert_eq!(
            text.matches(&format!("{before}{spelled}{after}")).count(),
            1,
            "{relative} must claim {before}{spelled}{after} exactly once"
        );
        for stale in CARDINALS.iter().filter(|word| **word != spelled) {
            assert!(
                !text.contains(&format!("{before}{stale}{after}")),
                "{relative} still spells {stale} where the patch adds {spelled}"
            );
        }
    }
}

/// Every lane runs the whole derived set, and no file restates a name.
///
/// The two halves are the repair. `--exact` runs zero tests and exits zero for
/// a name that is not written down, so fourteen `--exact` cells per lane meant
/// a fifteenth regression compiled everywhere and executed nowhere. One
/// derived MODULE filter per lane runs whatever the module holds, and the tree
/// scan refuses any file outside the patch's own crate and
/// [`UPSTREAM_REPORT_HOMES`] the right to name a regression -- so the list that
/// used to drift cannot be written again.
#[test]
fn every_gate_lane_runs_the_derived_regression_module_and_no_file_restates_a_name() {
    let root = workspace_root();
    let patched = fs::read(vendor_root().join(PATCH_REGRESSION_SOURCE))
        .expect("published attach test source is required");
    let derived = patch_regression_names(&patched);
    let filter = patch_regression_module_filter();

    let manifest: Value = serde_json::from_str(&read_workspace_text(REGRESSION_LANES[0]))
        .expect("the candidate phase manifest must be valid JSON");
    let mut carriers = Vec::new();
    for cells in manifest["phases"]
        .as_object()
        .expect("the manifest declares phases")
        .values()
    {
        for cell in cells.as_array().expect("a phase is a list of cells") {
            let argv = cell["argv"]
                .as_array()
                .expect("a cell declares an argv")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            if !argv.contains(&filter.as_str()) {
                continue;
            }
            let id = cell["id"].as_str().expect("a cell declares an id");
            for demanded in [
                "--manifest-path",
                "vendor/rmux-server/Cargo.toml",
                "--lib",
                "--no-default-features",
                "--test-threads=1",
            ] {
                assert!(
                    argv.contains(&demanded),
                    "cell {id} filters {filter} without {demanded}"
                );
            }
            assert!(
                !argv.contains(&"--exact"),
                "cell {id} pairs the module filter with --exact, which turns it \
                 into a name nobody wrote and runs nothing"
            );
            carriers.push(id.to_owned());
        }
    }
    assert_eq!(
        carriers.len(),
        1,
        "exactly one candidate cell must run the patch regression module, got {carriers:?}"
    );
    let gate = &carriers[0];

    let projection: Value = serde_json::from_str(&read_workspace_text(LINUX_GATE_MANIFEST))
        .expect("the Linux gate manifest must be valid JSON");
    assert!(
        projection["gates"]
            .as_array()
            .expect("the Linux manifest declares gates")
            .iter()
            .any(|declared| declared["name"].as_str() == Some(gate.as_str())),
        "{LINUX_GATE_MANIFEST} does not declare {gate}"
    );

    for lane in REGRESSION_LANES {
        let text = read_workspace_text(lane);
        assert!(
            text.contains(&filter),
            "{lane} does not run the derived regression module {filter}"
        );
    }
    assert!(
        read_workspace_text(REGRESSION_LANES[1]).contains(&format!("run_gate {gate}")),
        "{} does not run the {gate} gate",
        REGRESSION_LANES[1]
    );

    let mut files = Vec::new();
    scannable_files(&root, &mut files);
    assert!(!files.is_empty(), "the tree scan found nothing");
    // Two boundaries rather than a file list, and compared by prefix: a third
    // file added inside either one needs no edit here, which is the whole of
    // what "derived" buys a set somebody would otherwise maintain by hand.
    let homes = std::iter::once(vendor_root())
        .chain(UPSTREAM_REPORT_HOMES.iter().map(|home| root.join(home)))
        .collect::<BTreeSet<_>>();
    let published = homes
        .iter()
        .map(|home| {
            home.strip_prefix(&root)
                .unwrap_or(home)
                .display()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(", ");
    for path in files {
        if homes.iter().any(|home| path.starts_with(home)) {
            continue;
        }
        let text = String::from_utf8_lossy(
            &fs::read(&path)
                .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display())),
        )
        .into_owned();
        for name in &derived {
            assert!(
                !text.contains(name.as_str()),
                "{} restates the patch regression {name}; the names belong \
                 under {published} and nowhere else",
                path.display()
            );
        }
    }
}

#[test]
fn locked_cargo_graph_resolves_exactly_the_vendored_rmux_server() {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = Command::new(cargo)
        .args(["metadata", "--offline", "--locked", "--format-version", "1"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata must launch");
    assert!(
        output.status.success(),
        "locked offline cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata must be valid JSON");
    let packages = metadata["packages"]
        .as_array()
        .expect("metadata packages must be an array");
    let rmux_packages = packages
        .iter()
        .filter(|package| package["name"].as_str() == Some(CRATE_NAME))
        .collect::<Vec<_>>();
    assert_eq!(
        rmux_packages.len(),
        1,
        "exactly one rmux-server must resolve"
    );
    let rmux = rmux_packages[0];
    assert_eq!(rmux["version"].as_str(), Some(CRATE_VERSION));
    assert!(
        rmux.get("source").is_some_and(Value::is_null),
        "rmux-server must be a path package"
    );
    let expected_manifest = vendor_root()
        .join("Cargo.toml")
        .canonicalize()
        .expect("vendor manifest must resolve");
    let resolved_manifest = PathBuf::from(
        rmux["manifest_path"]
            .as_str()
            .expect("rmux-server manifest_path must be text"),
    )
    .canonicalize()
    .expect("resolved rmux-server manifest must exist");
    assert_eq!(resolved_manifest, expected_manifest);
    let rmux_id = rmux["id"].as_str().expect("rmux-server ID must be text");
    assert!(
        !metadata["workspace_members"]
            .as_array()
            .expect("workspace_members must be an array")
            .iter()
            .any(|member| member.as_str() == Some(rmux_id)),
        "the pristine vendored package must remain excluded from workspace mutation"
    );

    let consumer = packages
        .iter()
        .find(|package| package["name"].as_str() == Some("pmux-rmuxd"))
        .expect("pmux-rmuxd package must resolve");
    let dependency = consumer["dependencies"]
        .as_array()
        .expect("consumer dependencies must be an array")
        .iter()
        .find(|dependency| dependency["name"].as_str() == Some(CRATE_NAME))
        .expect("pmux-rmuxd must declare rmux-server");
    assert_eq!(dependency["req"].as_str(), Some("=0.9.0"));
    assert_eq!(
        dependency["uses_default_features"].as_bool(),
        Some(false),
        "pmux-rmuxd must disable rmux-server default features"
    );
    assert!(
        dependency["features"].as_array().is_some_and(Vec::is_empty),
        "pmux-rmuxd must not request an explicit rmux-server feature"
    );

    let consumer_id = consumer["id"].as_str().expect("pmux-rmuxd ID must be text");
    let consumer_node = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve nodes must be an array")
        .iter()
        .find(|node| node["id"].as_str() == Some(consumer_id))
        .expect("pmux-rmuxd resolve node must exist");
    let resolved_dependency = consumer_node["deps"]
        .as_array()
        .expect("resolve deps must be an array")
        .iter()
        .find(|dependency| dependency["name"].as_str() == Some("rmux_server"))
        .expect("resolved rmux-server dependency must exist");
    assert_eq!(resolved_dependency["pkg"].as_str(), Some(rmux_id));

    let rmux_node = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve nodes must be an array")
        .iter()
        .find(|node| node["id"].as_str() == Some(rmux_id))
        .expect("resolved rmux-server node must exist");
    let resolved_features = rmux_node["features"]
        .as_array()
        .expect("resolved rmux-server features must be an array");
    assert!(
        resolved_features.is_empty(),
        "the product graph must resolve rmux-server with no features, got {resolved_features:?}"
    );
}
