#![cfg(unix)]
#![allow(
    unsafe_code,
    reason = "the process-boundary test creates a private PTY, inspects termios, and signals only exact retained child pids"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pseudomux_client::{ClientError, PmuxClient};
use pseudomux_e2e::{
    TEST_ANTHROPIC_SECRET, TEST_ATTESTATION_VERSION, TEST_ENV_ATTESTATION_MARKER,
    TEST_ENV_PATCHED_VALUE, TEST_ENV_SAFE_CONFIG_VALUE, TEST_ENV_SET_ONLY_VALUE,
    TEST_LAUNCH_SECRET, TEST_PROVIDER_SECRET, TEST_SUBSCRIPTION_KEYS, TEST_TRANSPARENT_EXACT_KEYS,
};
use pseudomux_protocol::v1::{
    AttachSessionRequest, AuthPolicy, CancelOutcome, ClaudeLaunchConfig, ClosePolicy,
    CompatibilityPolicy, CompletionAuthority, ConfigIsolation, ConfigSource, DaemonDiagnosis,
    DisconnectAction, EffortLevel, EnvironmentSpec, ErrorCode, EventPayload, HealthLayerName,
    InputTransport, LayerFinding, LifecycleMode, MAX_NATIVE_FRAME_BYTES, MessageBlock,
    PermissionMode, ProbeOutcome, Request, RequestEnvelope, ResponseEnvelope, ResponsePayload,
    RetentionPolicy, RunOnceRequest, RuntimeFinding, SessionCell, SessionFinding,
    SessionGenerationId, SessionHandle, SessionIdentity, SessionState, StartSessionRequest,
    StopReasonKind, SubscribeEventsRequest, SystemPromptPolicy, TerminalProfile, TerminalSpec,
    ToolStatus, TurnAccepted, TurnLeasePolicy, TurnOutcome, TurnRequest, TurnResult,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::Barrier;
use tokio::task::JoinSet;
use tokio::time::Instant;
use uuid::Uuid;

const PROFILE_VERSION: &str = "9.9.9";
const FIRST_PROMPT: &str = "PMUX_TEST_FIRST";
const RICH_RESULT_PROMPT: &str = "PMUX_TEST_RICH_RESULT_WITH_HISTORY";
const RESERVED_LEASE_PROMPT: &str = "PMUX_TEST_RESERVED_LEASE_MUST_NOT_REACH_CLAUDE";
const CANCEL_PROMPT: &str = "PMUX_TEST_CANCEL_HOLD";
const AFTER_CANCEL_PROMPT: &str = "PMUX_TEST_AFTER_CANCEL";
const RESUME_PROMPT: &str = "PMUX_TEST_RESUMED";
const HYBRID_PROMPT: &str = "PMUX_TEST_HYBRID";
const AMBIGUOUS_PASTE_PROMPT: &str = "PMUX_TEST_AMBIGUOUS_PASTE_NO_ENTER";
const ADMISSION_MODAL_PROMPT: &str = "PMUX_TEST_ADMISSION_MODAL_PERMISSION";
const POST_ENTER_MODAL_PROMPT: &str = "PMUX_TEST_POST_ENTER_MODAL_PERMISSION";
const RESTART_INTERRUPTED_PROMPT: &str = "PMUX_TEST_CANCEL_RESTART_INTERRUPTED_HOLD";
const RESTART_RESUMED_PROMPT: &str = "PMUX_TEST_RESTART_EXPLICIT_RESUME";
const RUN_ONCE_PROMPT: &str = "PMUX_TEST_RUN_ONCE";
const CLI_PROMPT: &str = "PMUX_TEST_CLI_RUN";
const MCP_PROMPT: &str = "PMUX_TEST_MCP_RUN_ONCE";
const FACADE_PROMPT: &str = "PMUX_TEST_FACADE_RUN_ONCE";
// 43 because the facade is exercised twice: once positionally with
// `stream-json`, and once in the campaign's own shape (`-p`, prompt on stdin,
// `--output-format json`).
const EXPECTED_CLAUDE_LAUNCHES: usize = 43;
// The full-stack matrix deliberately crosses real process, PTY, filesystem,
// and external-language-runtime boundaries. Exact immutable-deadline behavior
// has deterministic clock/fault coverage in the service suite. Result
// observation gets a fixed post-deadline grace period for terminal event
// publication, but its monotonic budget starts before submission retries so it
// cannot silently extend the semantic turn deadline.
const FAKE_TURN_DEADLINE_MS: u64 = 30_000;
const RESULT_OBSERVER_GRACE_MS: u64 = 10_000;
const RESULT_OBSERVER_BUDGET_MS: u64 = FAKE_TURN_DEADLINE_MS + RESULT_OBSERVER_GRACE_MS;
const RESULT_SUBSCRIBE_WAIT_MS: u64 = 1_000;
const TYPESCRIPT_DIST_PACKAGE: &[u8] = b"{\"type\":\"module\"}\n";

const TYPESCRIPT_CLIENT_ASSETS: &[&str] = &[
    "package.json",
    "src/client.ts",
    "src/index.ts",
    "src/protocol.ts",
    "src/smithers.ts",
    "tests/actual_daemon_e2e.mjs",
    "tests/dist-stage.mjs",
    "dist/client.d.ts",
    "dist/client.d.ts.map",
    "dist/client.js",
    "dist/client.js.map",
    "dist/index.d.ts",
    "dist/index.d.ts.map",
    "dist/index.js",
    "dist/index.js.map",
    "dist/package.json",
    "dist/protocol.d.ts",
    "dist/protocol.d.ts.map",
    "dist/protocol.js",
    "dist/protocol.js.map",
    "dist/smithers.d.ts",
    "dist/smithers.d.ts.map",
    "dist/smithers.js",
    "dist/smithers.js.map",
];
const PYTHON_CLIENT_ASSETS: &[&str] = &[
    "pyproject.toml",
    "pmux_client/__init__.py",
    "pmux_client/client.py",
    "pmux_client/protocol.py",
    "pmux_client/py.typed",
    "pmux_client/smithers.py",
    "tests/actual_daemon_e2e.py",
];

#[test]
fn external_typescript_stage_contract_rejects_invalid_roots_membership_modes_and_aliases() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let valid = typescript_dist_fixture();
    let valid_root = valid.path().canonicalize().unwrap();
    assert_eq!(
        validate_external_directory(&workspace, "test TypeScript dist", &valid_root),
        valid_root
    );
    validate_typescript_dist_root(&valid_root);

    assert_panics(|| {
        validate_external_directory(
            &workspace,
            "relative TypeScript dist",
            Path::new("relative/dist"),
        );
    });
    let noncanonical = valid_root.join("..").join(valid_root.file_name().unwrap());
    assert_panics(|| {
        validate_external_directory(&workspace, "noncanonical TypeScript dist", &noncanonical);
    });
    assert_panics(|| {
        validate_external_directory(
            &workspace,
            "in-workspace TypeScript dist",
            &workspace.join("clients/typescript/tests"),
        );
    });

    let link_parent = tempfile::tempdir().unwrap();
    let linked_root = link_parent.path().join("linked-dist");
    std::os::unix::fs::symlink(&valid_root, &linked_root).unwrap();
    assert_panics(|| {
        validate_external_directory(&workspace, "symlink TypeScript dist", &linked_root);
    });

    for mutation in [
        "missing",
        "extra",
        "mode",
        "hardlink",
        "directory",
        "symlink",
    ] {
        let fixture = typescript_dist_fixture();
        let root = fixture.path().canonicalize().unwrap();
        match mutation {
            "missing" => std::fs::remove_file(root.join("client.js")).unwrap(),
            "extra" => write_private_file(&root.join("extra.js"), b"export {};\n"),
            "mode" => std::fs::set_permissions(
                root.join("client.js"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap(),
            "hardlink" => {
                std::fs::remove_file(root.join("client.js")).unwrap();
                std::fs::hard_link(root.join("index.js"), root.join("client.js")).unwrap();
            }
            "directory" => {
                std::fs::remove_file(root.join("client.js")).unwrap();
                std::fs::create_dir(root.join("client.js")).unwrap();
            }
            "symlink" => {
                std::fs::remove_file(root.join("client.js")).unwrap();
                std::os::unix::fs::symlink(root.join("index.js"), root.join("client.js")).unwrap();
            }
            _ => unreachable!(),
        }
        assert_panics(|| validate_typescript_dist_root(&root));
    }
}

fn assert_panics(operation: impl FnOnce() + std::panic::UnwindSafe) {
    assert!(std::panic::catch_unwind(operation).is_err());
}

fn typescript_dist_fixture() -> TempDir {
    let fixture = tempfile::tempdir().unwrap();
    std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    for relative in TYPESCRIPT_CLIENT_ASSETS
        .iter()
        .filter_map(|relative| relative.strip_prefix("dist/"))
    {
        let bytes = if relative == "package.json" {
            TYPESCRIPT_DIST_PACKAGE
        } else {
            b"generated\n"
        };
        write_private_file(&fixture.path().join(relative), bytes);
    }
    fixture
}

fn write_private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "launches exact candidate daemon/private-rmux binaries and a credential-free fake Claude version probe"]
async fn actual_daemon_empty_profile_registry_rejects_without_launching_a_child() {
    let binaries = CandidateBinaries::from_environment();
    let sandbox = Sandbox::new(&binaries);
    let mut daemon = DaemonGuard::start_without_tested_profile(&binaries, &sandbox).await;
    daemon.assert_identity(&binaries, &sandbox);
    let client = PmuxClient::new(&sandbox.public_socket).unwrap();

    let before_launches = sandbox.launch_count();
    let error = client
        .start_session(sandbox.start_request(
            SessionIdentity::New {
                session_id: Some(Uuid::new_v4()),
            },
            RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000,
            },
        ))
        .await
        .unwrap_err();
    assert_server_code(error, ErrorCode::UnsupportedClaudeVersion);
    assert_eq!(sandbox.launch_count(), before_launches);
    assert_eq!(socket_identities_under(&sandbox.runtime_parent).len(), 2);

    // AND THE DAEMON IS HEALTHY. This configuration -- no promoted profile, no
    // pool -- is correct and supported, and it is the one `pmux doctor` exited
    // 1 on forever: the compatibility layer answered `not_established` for an
    // empty registry regardless of whether anything on the daemon needed one.
    // Nothing here does. The pool is what makes a promoted cell mandatory, and
    // there is no pool, so the layer has no subject rather than an unreachable
    // one -- while a caller who explicitly demands a tested cell is refused at
    // that request, which is what the refusal above just measured.
    let diagnosis = client.diagnose().await.unwrap();
    let compatibility = diagnosis
        .layer(HealthLayerName::CompatibilityProfile)
        .expect("the compatibility layer is reported");
    assert_eq!(
        compatibility.finding,
        LayerFinding::NothingToExercise,
        "a daemon that needs no promoted cell must not report one as unproven: {compatibility:?}"
    );
    assert!(
        diagnosis.missing_layers().is_empty(),
        "{:?}",
        diagnosis.missing_layers()
    );
    assert_eq!(
        diagnosis.outcome(),
        ProbeOutcome::Pass,
        "a correct Path A daemon rolls up healthy; layers: {:?}",
        diagnosis
            .layers
            .iter()
            .map(|layer| (layer.layer, layer.finding, layer.outcome))
            .collect::<Vec<_>>()
    );

    daemon.stop().await;
    binaries.assert_unchanged();
    assert!(!sandbox.public_socket.exists());
    assert_eq!(
        std::fs::read_dir(&sandbox.runtime_parent).unwrap().count(),
        0
    );
    assert_eq!(sandbox.launch_count(), 0);
}

/// The Path B acceptance sequence, end to end over the real Unix socket, with a
/// real private rmux PTY and a real external Claude process.
///
/// This is the test the whole wire surface exists for. Before it, no caller
/// could reach the minified cell at all: `select_minified_cell` required
/// `compatibility.tested`, `--compatibility allow-untested` could only ever
/// produce `tested: false`, and neither cell selection nor `/clear` had a
/// request variant. So the two escape hatches were mutually exclusive and Path
/// B was unreachable on every host. The fix is a *deployment* one -- admit a
/// profile with `--tested-claude-profile` -- plus the field and the method
/// exercised here.
///
/// Steps 7, 8 and 8c are the ones that distinguish this design from a naive
/// one: every stale fence is refused rather than answered or silently obeyed,
/// the recovery from a lost response is a snapshot read rather than an
/// inference, and a writable terminal attachment -- the one authenticated
/// channel that would let a second party type into this cell without the daemon
/// seeing it -- is refused outright.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "launches exact candidate binaries, a private real rmux PTY, and a credential-free fake Claude"]
async fn path_b_drives_a_minified_cell_through_clear_over_the_real_socket() {
    let binaries = CandidateBinaries::from_environment();
    let sandbox = Sandbox::new(&binaries);
    let mut daemon = DaemonGuard::start(&binaries, &sandbox).await;
    daemon.assert_identity(&binaries, &sandbox);
    let client = PmuxClient::new(&sandbox.public_socket).unwrap();

    // 0. The gate is deployment, not code: an untested profile must not reach
    //    the minified cell, and must refuse before a child is launched.
    let untested_sandbox = Sandbox::new(&binaries);
    let mut untested_daemon =
        DaemonGuard::start_without_tested_profile(&binaries, &untested_sandbox).await;
    let mut untested = untested_sandbox.start_request(
        SessionIdentity::New {
            session_id: Some(Uuid::new_v4()),
        },
        RetentionPolicy::Persistent {
            idle_ttl_ms: 60_000,
        },
    );
    untested.cell = SessionCell::Minified;
    untested.config_isolation = Some(ConfigIsolation {
        root: untested_sandbox
            .private_config_root("untested")
            .to_string_lossy()
            .into_owned(),
    });
    let refused = client_for(&untested_sandbox)
        .start_session(untested)
        .await
        .unwrap_err();
    assert_server_code(refused, ErrorCode::UnsupportedClaudeVersion);
    assert_eq!(
        untested_sandbox.launch_count(),
        0,
        "an inadmissible cell must refuse before a Claude process exists"
    );
    untested_daemon.stop().await;

    // 1. start_session with `cell: minified` on the admitted profile.
    let session_id = Uuid::new_v4();
    let mut start = sandbox.start_request(
        SessionIdentity::New {
            session_id: Some(session_id),
        },
        RetentionPolicy::Persistent {
            idle_ttl_ms: 1_800_000,
        },
    );
    start.cell = SessionCell::Minified;
    // Mandatory for this cell, and refused without it. A minified cell under a
    // shared root is a contradiction: the statelessness claim is about what one
    // instance carries between callers, and `history.jsonl`, `paste-cache/` and
    // `projects/` are all per-ROOT.
    let private_root = sandbox.private_config_root("cell");
    start.config_isolation = Some(ConfigIsolation {
        root: private_root.to_string_lossy().into_owned(),
    });
    start.claude.as_mut().expect("inline launch").denied_tools = vec!["*".into()];
    start
        .claude
        .as_mut()
        .expect("inline launch")
        .permission_mode = Some(PermissionMode::DontAsk);
    start.claude.as_mut().expect("inline launch").system_prompt = SystemPromptPolicy::Replace {
        prompt: "You are a bounded Path B cell.".into(),
    };
    let handle = client.start_session(start).await.unwrap();
    assert_eq!(handle.session_id, session_id);
    assert!(handle.compatibility.tested);
    let launch = sandbox.only_launch_for_session(session_id);
    let argv = launch["argv"].as_array().unwrap();
    assert!(
        argv.iter().any(|argument| argument == "--disallowedTools")
            && argv.iter().any(|argument| argument == "*"),
        "the minified cell must reach Claude with an empty tool surface: {argv:?}"
    );

    // 2-3. One turn, proven from the transcript exactly as the Full cell is.
    let first = submit_turn(&client, &handle, FIRST_PROMPT).await;
    let first_result = wait_for_result(&client, &handle, &first, "path_b_first").await;
    assert_completed(&first_result, session_id, first.turn_id);

    // 4. clear_session, fenced on the id the session started bound to.
    let cleared = client
        .clear_session(session_id, handle.generation_id, session_id, None)
        .await
        .unwrap();
    assert!(cleared.rotated, "the first clear must rotate");
    assert_eq!(
        cleared.session_id, session_id,
        "the caller's handle is stable"
    );
    assert_eq!(cleared.generation_id, handle.generation_id);
    assert_ne!(
        cleared.transcript_session_id, session_id,
        "the clear must rotate Claude's own id"
    );
    let first_rotation = cleared.transcript_session_id;

    // 5. A second turn runs under the rotated transcript. That it completes at
    //    all is the proof that the tail followed the rotation and re-armed.
    let second = submit_turn(&client, &handle, FIRST_PROMPT).await;
    let second_result = wait_for_result(&client, &handle, &second, "path_b_second").await;
    assert_completed(&second_result, session_id, second.turn_id);

    // 6. A second clear, fenced on what the first one returned.
    let cleared_again = client
        .clear_session(session_id, handle.generation_id, first_rotation, None)
        .await
        .unwrap();
    assert!(cleared_again.rotated);
    let second_rotation = cleared_again.transcript_session_id;
    assert_ne!(second_rotation, first_rotation);

    // 7. The same request bytes again. Refused, and nothing is typed. A re-send
    //    is one rotation stale, which is exactly what a second caller holding
    //    the fence a session STARTS with also looks like, so there is no answer
    //    that is right for both; the daemon gives neither one an "already
    //    cleared".
    let replay = client
        .clear_session(session_id, handle.generation_id, first_rotation, None)
        .await
        .unwrap_err();
    let ClientError::Server(body) = replay else {
        panic!("a stale fence must be refused by the server");
    };
    assert_eq!(body.code, ErrorCode::IdConflict);
    assert_eq!(body.details["violation"], "stale_transcript_fence");

    // 8. Two rotations stale is refused by the same rule and the same code:
    //    there is one fence rule, not a graded one.
    let conflict = client
        .clear_session(session_id, handle.generation_id, session_id, None)
        .await
        .unwrap_err();
    assert_server_code(conflict, ErrorCode::IdConflict);

    // File-level evidence, because "the calls returned" is not statelessness.
    // Each clear opened exactly one new transcript and abandoned the previous
    // one untouched: turn 1 stays in the launch file, turn 2 stays in the file
    // the first clear opened, and the file the second clear opened carries only
    // the preamble -- no prompt, no reply, no completion marker.
    let project = private_root.join("projects/pmux-e2e");
    let transcripts = std::fs::read_dir(&project)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        transcripts.len(),
        3,
        "two clears must open exactly two new transcripts: {transcripts:?}"
    );
    let launch_transcript = std::fs::read_to_string(project.join(format!("{session_id}.jsonl")))
        .expect("the abandoned launch transcript is left on disk untouched");
    assert!(launch_transcript.contains(FIRST_PROMPT));
    let first_rotation_transcript =
        std::fs::read_to_string(project.join(format!("{first_rotation}.jsonl"))).unwrap();
    assert!(
        first_rotation_transcript.contains(FIRST_PROMPT),
        "turn 2 must have been proven from the transcript the first clear opened"
    );
    let second_rotation_transcript =
        std::fs::read_to_string(project.join(format!("{second_rotation}.jsonl"))).unwrap();
    assert!(
        !second_rotation_transcript.contains(FIRST_PROMPT),
        "a cleared cell must carry no prompt from the caller before it"
    );
    assert!(
        second_rotation_transcript.contains("<command-name>/clear</command-name>"),
        "the rebound transcript must carry Claude's own record of which command ran"
    );
    assert!(
        !second_rotation_transcript.contains("turn_duration"),
        "a cleared cell must carry no completion marker"
    );

    // The session survived every refusal above and is still usable, and it
    // publishes the fence rather than making the caller re-derive it: a caller
    // that lost `expected_transcript_session_id` reads it back here, which is
    // the whole reason a lost fence must not be reconstructed from bookkeeping.
    let snapshot = client
        .inspect_session(session_id, handle.generation_id)
        .await
        .unwrap();
    assert_eq!(
        snapshot.state,
        pseudomux_protocol::v1::SessionState::Ready,
        "a refused fence must not taint the session"
    );
    assert_eq!(
        snapshot.transcript_session_id, second_rotation,
        "the snapshot must publish the transcript the next clear has to be fenced on"
    );
    assert_eq!(snapshot.cell, SessionCell::Minified);

    // 8b. The recovery a caller actually has, over the socket, end to end. A
    //     caller whose response was lost reads the fence back and clears on it;
    //     that succeeds, and it is the ONLY success shape -- `rotated` is true
    //     because a clear that returns a result is a clear that ran. Nothing
    //     ever reports a clear as already done.
    let third = submit_turn(&client, &handle, FIRST_PROMPT).await;
    let third_result = wait_for_result(&client, &handle, &third, "path_b_third").await;
    assert_completed(&third_result, session_id, third.turn_id);
    let stale_again = client
        .clear_session(session_id, handle.generation_id, first_rotation, None)
        .await
        .unwrap_err();
    let ClientError::Server(body) = stale_again else {
        panic!("a stale fence must be refused by the server");
    };
    assert_eq!(body.code, ErrorCode::IdConflict);
    assert_eq!(body.details["violation"], "stale_transcript_fence");
    let unchanged = client
        .inspect_session(session_id, handle.generation_id)
        .await
        .unwrap();
    assert_eq!(
        unchanged.transcript_session_id, second_rotation,
        "a refused fence must not rotate the transcript"
    );
    let recovered = client
        .clear_session(
            session_id,
            handle.generation_id,
            unchanged.transcript_session_id,
            None,
        )
        .await
        .unwrap();
    assert!(
        recovered.rotated,
        "every clear that returns a result rotated; there is no other success"
    );
    assert_ne!(recovered.transcript_session_id, second_rotation);

    // 8c. A writable attach is refused on this cell, before any rmux grant is
    //     minted. It is the one authenticated channel that could put a second
    //     party's keystrokes into the TUI without the daemon ever seeing them --
    //     composer text that would prefix the next caller's prompt, up-arrow
    //     recall out of this root's own `history.jsonl`, a hand-typed `/clear`
    //     that rotates Claude's id underneath the bound one.
    let attach = client
        .attach_session(AttachSessionRequest {
            session_id,
            generation_id: handle.generation_id,
            read_only: false,
            size: None,
        })
        .await
        .unwrap_err();
    let ClientError::Server(body) = attach else {
        panic!("a writable attach on a minified cell must be refused by the server");
    };
    assert_eq!(body.code, ErrorCode::UnsupportedFeature);
    assert_eq!(
        body.details["violation"],
        "writable_attach_forbidden_on_minified_cell"
    );

    // 9. Close, with the process boundary proven empty.
    let closed = client
        .close_session(session_id, handle.generation_id, ClosePolicy::Graceful)
        .await
        .unwrap();
    assert!(closed.process_reaped);
    assert_process_boundary_absent(&launch, "Path B minified cell retained a Claude process");

    // 10. A stateless cell must not silently be a resumed one -- and under the
    //     per-cell root mandate it cannot get far enough to be one. `resume`
    //     names a transcript that already holds a prior caller's context, and
    //     that transcript lives inside a root which by then also holds this
    //     cell's `history.jsonl`, its `backups/` and every abandoned transcript
    //     the two clears left. So the refusal arrives at the ROOT rule, ahead of
    //     the launch half of assert-empty, and ahead of any child.
    //
    //     Assert-empty-at-launch is not thereby untested, and is not thereby
    //     dead: it lives at `SessionRegistry::register`, which is `pub` and
    //     which an embedder reaches without any config root at all, and it is
    //     proven there by
    //     `minified_cell.rs::a_minified_cell_cannot_be_registered_over_a_transcript_that_served_work`
    //     plus the polarity test on its refusing default. What changed is which
    //     of two independent rules refuses first over the wire.
    let launches_before = sandbox.launch_count();
    let mut resumed = sandbox.start_request(
        SessionIdentity::Resume { session_id },
        RetentionPolicy::Persistent {
            idle_ttl_ms: 60_000,
        },
    );
    resumed.cell = SessionCell::Minified;
    resumed.config_isolation = Some(ConfigIsolation {
        root: private_root.to_string_lossy().into_owned(),
    });
    let leaked = client.start_session(resumed).await.unwrap_err();
    let ClientError::Server(body) = leaked else {
        panic!("a resumed minified cell must be refused by the server");
    };
    assert_eq!(body.code, ErrorCode::InvalidConfig);
    assert!(
        body.message.contains("root it alone has ever used"),
        "the refusal must name the rule it applied: {body:?}"
    );
    assert!(
        !format!("{body:?}").contains(FIRST_PROMPT),
        "a refusal must disclose no transcript content"
    );
    assert_eq!(
        sandbox.launch_count(),
        launches_before,
        "a refused minified start must not launch a child"
    );

    // 10b. And a minified start with no private root at all is refused on the
    //      request alone. A shared root makes the statelessness claim false at
    //      the storage layer -- `history.jsonl`, `paste-cache/` and `projects/`
    //      are per-ROOT -- however clean any one transcript is.
    let mut rootless = sandbox.start_request(
        SessionIdentity::New {
            session_id: Some(Uuid::new_v4()),
        },
        RetentionPolicy::Persistent {
            idle_ttl_ms: 60_000,
        },
    );
    rootless.cell = SessionCell::Minified;
    let refused_rootless = client.start_session(rootless).await.unwrap_err();
    let ClientError::Server(body) = refused_rootless else {
        panic!("a minified cell without config isolation must be refused by the server");
    };
    assert_eq!(body.code, ErrorCode::InvalidConfig);
    assert!(
        body.message.contains("requires config_isolation"),
        "the refusal must name the missing field: {body:?}"
    );
    assert_eq!(
        sandbox.launch_count(),
        launches_before,
        "a refused minified start must not launch a child"
    );

    // 10c. The two refusals above are about the CELL, not about the root or the
    //      resume: the same used root and the same populated transcript are
    //      admitted for a Full cell, which is entitled to its own accumulation.
    let mut held = sandbox.start_request(
        SessionIdentity::Resume { session_id },
        RetentionPolicy::Persistent {
            idle_ttl_ms: 60_000,
        },
    );
    held.config_isolation = Some(ConfigIsolation {
        root: private_root.to_string_lossy().into_owned(),
    });
    let held_handle = client
        .start_session(held)
        .await
        .expect("a Full cell may resume inside a root it has already used");
    client
        .close_session(
            held_handle.session_id,
            held_handle.generation_id,
            ClosePolicy::Graceful,
        )
        .await
        .unwrap();

    daemon.stop().await;
    binaries.assert_unchanged();
}

fn client_for(sandbox: &Sandbox) -> PmuxClient {
    PmuxClient::new(&sandbox.public_socket).unwrap()
}

/// Public-boundary counterpart to the lower-level process observer regression.
/// The fake Claude starts one direct descendant in the same isolated POSIX
/// session. Rmux cleanup first observes that tree, then its PTY HUP causes the
/// descendant to call `setsid(2)`. Both public close attempts must remain
/// retryable and unconfirmed, and pmux must not signal the escaped identity.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "launches exact candidate daemon/private-rmux binaries and a controlled escaping descendant"]
async fn public_close_retry_never_claims_an_observed_escaped_descendant_was_reaped() {
    let binaries = CandidateBinaries::from_environment();
    let sandbox = Sandbox::new(&binaries);
    let mut daemon = DaemonGuard::start(&binaries, &sandbox).await;
    daemon.assert_identity(&binaries, &sandbox);
    let client = PmuxClient::new(&sandbox.public_socket).unwrap();

    let session_id = Uuid::new_v4();
    let mut request = sandbox.start_request(
        SessionIdentity::New {
            session_id: Some(session_id),
        },
        RetentionPolicy::Persistent {
            idle_ttl_ms: 60_000,
        },
    );
    request
        .environment
        .set
        .insert("PMUX_TEST_SPAWN_ESCAPING_DESCENDANT".into(), "1".into());
    let handle = client.start_session(request).await.unwrap();
    let launch = sandbox.only_launch_for_session(session_id);
    let mut launch_cleanup = ExactProcessCleanupGuard::new(exact_process_identity_from_launch(
        &launch,
        &binaries.fake_claude,
    ));
    let pid_file = sandbox
        .state_root
        .join(format!("escape-descendant-{session_id}.pid"));
    let ready_file = sandbox
        .state_root
        .join(format!("escape-descendant-{session_id}.ready"));
    let escaped_file = sandbox
        .state_root
        .join(format!("escape-descendant-{session_id}.escaped"));
    wait_for_file(&ready_file, Duration::from_secs(5)).await;
    let descendant_pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    let before_escape = ExactProcessIdentity::capture(descendant_pid, &binaries.fake_claude);
    let mut descendant_cleanup = ExactProcessCleanupGuard::new(before_escape.clone());
    let launch_session_id = launch["process_session_id"].as_i64().unwrap() as i32;
    assert_eq!(before_escape.session_id, launch_session_id);
    assert_ne!(
        before_escape.session_id,
        i32::try_from(descendant_pid).unwrap(),
        "the controlled descendant escaped before public close could observe it"
    );

    let first_error = client
        .close_session(
            handle.session_id,
            handle.generation_id,
            ClosePolicy::Graceful,
        )
        .await
        .unwrap_err();
    assert_retryable_server_code(first_error, ErrorCode::RecoveryFailed);
    wait_for_file(&escaped_file, Duration::from_secs(5)).await;
    let escaped = ExactProcessIdentity::capture(descendant_pid, &binaries.fake_claude);
    before_escape.assert_same_process(&escaped);
    descendant_cleanup.update(escaped.clone());
    assert_eq!(escaped.session_id, i32::try_from(descendant_pid).unwrap());
    assert_eq!(
        escaped.process_group_id,
        i32::try_from(descendant_pid).unwrap()
    );
    escaped.assert_running();

    let closing = client
        .inspect_session(handle.session_id, handle.generation_id)
        .await
        .unwrap();
    assert_eq!(closing.state, pseudomux_protocol::v1::SessionState::Closing);

    let second_error = client
        .close_session(handle.session_id, handle.generation_id, ClosePolicy::Force)
        .await
        .unwrap_err();
    assert_retryable_server_code(second_error, ErrorCode::RecoveryFailed);
    escaped.assert_running();

    // The product deliberately cannot signal a PID after it leaves the proved
    // session boundary. The test owns this one retained kernel identity and is
    // therefore the only component authorized to terminate it.
    descendant_cleanup.identity().signal(libc::SIGKILL);
    wait_for_exact_process_absence(descendant_cleanup.identity(), Duration::from_secs(10)).await;
    descendant_cleanup.disarm();
    assert_process_boundary_absent(&launch, "Claude boundary after descendant escape");
    launch_cleanup.disarm();

    daemon.stop_expecting_shutdown_failure(&sandbox).await;
    binaries.assert_unchanged();
    assert!(!sandbox.public_socket.exists());
    assert_eq!(
        std::fs::read_dir(&sandbox.runtime_parent).unwrap().count(),
        0
    );
}

/// Kills the exact private sidecar only after a public turn has been accepted
/// and acknowledged by the transcript. The public event must remain a typed
/// daemon-loss failure, while pmuxd's locally retained process boundary reaps
/// the active fake Claude before the session can close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "launches exact candidate binaries, SIGKILLs only their retained sidecar child, and uses no real Claude"]
async fn active_public_turn_sidecar_loss_is_typed_and_reaps_the_process_boundary() {
    let binaries = CandidateBinaries::from_environment();
    let sandbox = Sandbox::new(&binaries);
    let mut daemon = DaemonGuard::start(&binaries, &sandbox).await;
    daemon.assert_identity(&binaries, &sandbox);
    let client = PmuxClient::new(&sandbox.public_socket).unwrap();

    let session_id = Uuid::new_v4();
    let handle = client
        .start_session(sandbox.start_request(
            SessionIdentity::New {
                session_id: Some(session_id),
            },
            RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000,
            },
        ))
        .await
        .unwrap();
    let launch = sandbox.only_launch_for_session(session_id);
    let mut launch_cleanup = ExactProcessCleanupGuard::new(exact_process_identity_from_launch(
        &launch,
        &binaries.fake_claude,
    ));
    let accepted = submit_turn(&client, &handle, CANCEL_PROMPT).await;
    wait_for_prompt_ack(&client, &handle, &accepted).await;

    daemon.kill_exact_sidecar().await;
    let failure = wait_for_failure(&client, &handle, &accepted).await;
    assert_eq!(failure.code, ErrorCode::DaemonLost);
    assert!(failure.retryable);
    assert_process_boundary_absent(&launch, "active Claude process after exact sidecar loss");
    launch_cleanup.disarm();

    let snapshot = client
        .inspect_session(handle.session_id, handle.generation_id)
        .await
        .unwrap();
    assert_eq!(snapshot.state, pseudomux_protocol::v1::SessionState::Failed);
    assert_eq!(snapshot.last_turn.unwrap().turn_id, accepted.turn_id);
    let closed = client
        .close_session(handle.session_id, handle.generation_id, ClosePolicy::Force)
        .await
        .unwrap();
    assert!(closed.process_reaped);

    daemon.stop_expecting_shutdown_failure(&sandbox).await;
    binaries.assert_unchanged();
    assert!(!sandbox.public_socket.exists());
    assert_eq!(
        std::fs::read_dir(&sandbox.runtime_parent).unwrap().count(),
        0
    );
}

/// Every condition in which `pmux doctor` reported `"healthy": true` while the
/// daemon could not serve a single turn.
///
/// All four share one shape: `pmuxd`'s accept loop is perfect. `Request::Ping`
/// is answered in the first arm of `NativeService::dispatch` and that arm never
/// dereferences `self`, so the private runtime, the session registry, the
/// launch broker and the rmux sidecar are all untouched by it. Three of
/// `doctor`'s four old checks never left the client process at all, and the
/// fourth stopped at that arm. `healthy` was `errors.is_empty()` over the four.
///
/// This test asserts the contrast directly: after each fault, `ping` STILL
/// SUCCEEDS, and `diagnose` reports the fault. A probe that merely sampled
/// liveness would pass every assertion `ping` passes here.
///
/// Conditions, in order:
///
/// 1. the Claude child is killed while both daemons stay up (`terminal_missing`);
/// 2. a turn submitted into that session fails, and the session's own state
///    turns terminal, at which point the report says `session_declared_unusable`
///    rather than silently becoming healthy again;
/// 3. the sidecar is `SIGSTOP`ped (`control_plane_unresponsive`), and `SIGCONT`
///    restores `private_runtime_responsive`, so the probe is proven to be
///    measuring the fault and not latching on it;
/// 4. the sidecar is `SIGKILL`ed (`control_plane_unreachable`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "launches exact candidate binaries, SIGSTOPs and SIGKILLs only their retained sidecar child, and uses no real Claude"]
async fn doctor_probe_catches_every_fault_that_ping_answers_through() {
    let binaries = CandidateBinaries::from_environment();

    // --- conditions 1 and 2: the child dies, both daemons live -------------
    let sandbox = Sandbox::new(&binaries);
    let mut daemon = DaemonGuard::start(&binaries, &sandbox).await;
    daemon.assert_identity(&binaries, &sandbox);
    let client = PmuxClient::new(&sandbox.public_socket).unwrap();

    let session_id = Uuid::new_v4();
    let handle = client
        .start_session(sandbox.start_request(
            SessionIdentity::New {
                session_id: Some(session_id),
            },
            RetentionPolicy::Persistent {
                idle_ttl_ms: 600_000,
            },
        ))
        .await
        .unwrap();

    let healthy = client.diagnose().await.unwrap();
    assert_eq!(healthy.outcome(), ProbeOutcome::Pass);
    assert_eq!(
        healthy.runtime.finding,
        RuntimeFinding::PrivateRuntimeResponsive
    );
    assert_eq!(healthy.runtime.live_private_terminals, Some(1));
    assert_eq!(healthy.sessions.len(), 1);
    assert_eq!(healthy.sessions[0].session_id, handle.session_id);
    assert_eq!(healthy.sessions[0].finding, SessionFinding::TerminalPresent);

    let launch = sandbox.only_launch_for_session(session_id);
    let child = exact_process_identity_from_launch(&launch, &binaries.fake_claude);
    child.signal(libc::SIGKILL);

    let faulted = wait_for_diagnosis(&client, |diagnosis| {
        diagnosis.sessions.first().map(|session| session.finding)
            == Some(SessionFinding::TerminalMissing)
    })
    .await;
    // The daemon-level checks the old report folded over are all still fine.
    // That is the whole point: it is exactly the state that printed "healthy".
    assert!(client.ping().await.is_ok());
    assert_eq!(
        faulted.runtime.finding,
        RuntimeFinding::PrivateRuntimeResponsive
    );
    assert_eq!(faulted.runtime.live_private_terminals, Some(0));
    assert_eq!(faulted.sessions[0].outcome, ProbeOutcome::Fail);
    assert_eq!(faulted.sessions[0].private_terminal_present, Some(false));
    // No code polls an idle session's terminal, so pmux still offers this one.
    assert_eq!(faulted.sessions[0].state, Some(SessionState::Ready));
    assert_eq!(faulted.outcome(), ProbeOutcome::Fail);

    // Condition 2: the caller finds out the hard way. The report must not
    // become healthy again when the session's state turns terminal.
    let accepted = client
        .run_turn(
            handle.session_id,
            handle.generation_id,
            turn(Uuid::new_v4(), FIRST_PROMPT),
        )
        .await
        .unwrap();
    let failure = wait_for_failure(&client, &handle, &accepted).await;
    assert!(!failure.message.is_empty());
    let after_failure = wait_for_diagnosis(&client, |diagnosis| {
        diagnosis.sessions.first().map(|session| session.finding)
            == Some(SessionFinding::SessionDeclaredUnusable)
    })
    .await;
    assert!(client.ping().await.is_ok());
    assert_eq!(after_failure.sessions[0].outcome, ProbeOutcome::Unproven);
    assert_ne!(after_failure.outcome(), ProbeOutcome::Pass);

    client
        .close_session(handle.session_id, handle.generation_id, ClosePolicy::Force)
        .await
        .unwrap();
    daemon.stop().await;

    // --- conditions 3 and 4: the sidecar stops answering -------------------
    let sandbox = Sandbox::new(&binaries);
    let mut daemon = DaemonGuard::start(&binaries, &sandbox).await;
    daemon.assert_identity(&binaries, &sandbox);
    let client = PmuxClient::new(&sandbox.public_socket).unwrap();
    assert_eq!(
        client.diagnose().await.unwrap().outcome(),
        ProbeOutcome::Pass
    );

    daemon.signal_exact_sidecar(libc::SIGSTOP);
    let stopped = client.diagnose().await.unwrap();
    assert!(client.ping().await.is_ok());
    assert_eq!(
        stopped.runtime.finding,
        RuntimeFinding::ControlPlaneUnresponsive
    );
    assert_eq!(stopped.runtime.outcome, ProbeOutcome::Fail);
    assert_eq!(stopped.runtime.live_private_terminals, None);

    // Measuring, not latching: a resumed sidecar is healthy again.
    daemon.signal_exact_sidecar(libc::SIGCONT);
    let resumed = wait_for_diagnosis(&client, |diagnosis| {
        diagnosis.runtime.finding == RuntimeFinding::PrivateRuntimeResponsive
    })
    .await;
    assert_eq!(resumed.outcome(), ProbeOutcome::Pass);

    daemon.kill_exact_sidecar().await;
    let killed = client.diagnose().await.unwrap();
    assert!(client.ping().await.is_ok());
    assert_eq!(
        killed.runtime.finding,
        RuntimeFinding::ControlPlaneUnreachable
    );
    assert_eq!(killed.runtime.outcome, ProbeOutcome::Fail);
    assert_eq!(killed.outcome(), ProbeOutcome::Fail);

    daemon.stop_expecting_shutdown_failure(&sandbox).await;
    binaries.assert_unchanged();
}

/// Polls `diagnose` until `ready` holds, then returns the report that satisfied
/// it. Bounded, because a probe that never reports the fault is the bug.
async fn wait_for_diagnosis(
    client: &PmuxClient,
    ready: impl Fn(&DaemonDiagnosis) -> bool,
) -> DaemonDiagnosis {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let diagnosis = client.diagnose().await.unwrap();
        if ready(&diagnosis) {
            return diagnosis;
        }
        assert!(
            Instant::now() < deadline,
            "diagnose never reported the expected finding: {diagnosis:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Restart deliberately loses the in-memory actor registry. It must neither
/// reconstruct the interrupted actor nor launch/reinject work on its own;
/// only a caller-directed resume of the known transcript UUID may create a
/// fresh process generation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "launches exact candidate binaries, a private real rmux PTY, and a credential-free fake Claude"]
async fn daemon_restart_requires_explicit_resume_without_prompt_reinjection() {
    let binaries = CandidateBinaries::from_environment();
    let sandbox = Sandbox::new(&binaries);
    let session_id = Uuid::new_v4();

    let mut daemon = DaemonGuard::start(&binaries, &sandbox).await;
    daemon.assert_identity(&binaries, &sandbox);
    daemon.assert_no_inet_sockets();
    let client = PmuxClient::new(&sandbox.public_socket).unwrap();
    let original = client
        .start_session(sandbox.start_request(
            SessionIdentity::New {
                session_id: Some(session_id),
            },
            RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000,
            },
        ))
        .await
        .unwrap();
    let original_launch = sandbox.only_launch_for_session(session_id);
    let interrupted = submit_turn(&client, &original, RESTART_INTERRUPTED_PROMPT).await;
    wait_for_prompt_ack(&client, &original, &interrupted).await;

    let transcript = sandbox
        .config_root
        .join("projects/pmux-e2e")
        .join(format!("{session_id}.jsonl"));
    let before_restart = std::fs::read_to_string(&transcript).unwrap();
    assert_eq!(
        before_restart.matches(RESTART_INTERRUPTED_PROMPT).count(),
        1
    );
    assert_eq!(sandbox.launch_count(), 1);

    daemon.assert_no_inet_sockets();
    daemon.stop().await;
    assert_process_boundary_absent(
        &original_launch,
        "interrupted Claude process at daemon restart",
    );

    let mut restarted = DaemonGuard::start(&binaries, &sandbox).await;
    restarted.assert_identity(&binaries, &sandbox);
    restarted.assert_no_inet_sockets();
    let restarted_client = PmuxClient::new(&sandbox.public_socket).unwrap();
    restarted_client.ping().await.unwrap();

    let missing_actor = restarted_client
        .inspect_session(original.session_id, original.generation_id)
        .await
        .unwrap_err();
    assert_server_code(missing_actor, ErrorCode::SessionNotFound);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        sandbox.launch_count(),
        1,
        "daemon restart reconstructed or relaunched an actor without a public request"
    );
    let after_restart = std::fs::read_to_string(&transcript).unwrap();
    assert_eq!(after_restart, before_restart);
    assert_eq!(after_restart.matches(RESTART_INTERRUPTED_PROMPT).count(), 1);

    let resumed = restarted_client
        .start_session(sandbox.start_request(
            SessionIdentity::Resume { session_id },
            RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000,
            },
        ))
        .await
        .unwrap();
    assert_ne!(resumed.generation_id, original.generation_id);
    assert_eq!(sandbox.launch_count(), 2);
    let stale_after_resume = restarted_client
        .inspect_session(original.session_id, original.generation_id)
        .await
        .unwrap_err();
    assert_server_code(stale_after_resume, ErrorCode::StaleSessionGeneration);

    let resumed_launches = sandbox.launches_for_session(session_id);
    assert_eq!(resumed_launches.len(), 2);
    sandbox.assert_plain_launch(
        &binaries,
        &resumed_launches[1],
        session_id,
        "resume",
        &["--model", "test-model", "--permission-mode", "default"],
    );
    assert_eq!(
        std::fs::read_to_string(&transcript).unwrap(),
        before_restart,
        "explicit resume injected transcript input before a new public turn"
    );
    let resumed_turn = submit_turn(&restarted_client, &resumed, RESTART_RESUMED_PROMPT).await;
    let resumed_result = wait_for_result(
        &restarted_client,
        &resumed,
        &resumed_turn,
        RESTART_RESUMED_PROMPT,
    )
    .await;
    assert_completed(&resumed_result, session_id, resumed_turn.turn_id);
    let final_transcript = std::fs::read_to_string(&transcript).unwrap();
    assert_eq!(
        final_transcript.matches(RESTART_INTERRUPTED_PROMPT).count(),
        1
    );
    assert_eq!(final_transcript.matches(RESTART_RESUMED_PROMPT).count(), 1);
    let close = restarted_client
        .close_session(
            resumed.session_id,
            resumed.generation_id,
            ClosePolicy::Force,
        )
        .await
        .unwrap();
    assert!(close.process_reaped);
    assert_process_boundary_absent(&resumed_launches[1], "explicitly resumed Claude process");

    restarted.assert_no_inet_sockets();
    restarted.stop().await;
    binaries.assert_unchanged();
    assert!(!sandbox.public_socket.exists());
    assert_eq!(
        std::fs::read_dir(&sandbox.runtime_parent).unwrap().count(),
        0
    );
    for stderr in ["pmuxd.stderr", "pmuxd.stderr.1"] {
        let contents = std::fs::read_to_string(sandbox.root_path.join(stderr)).unwrap();
        assert!(!contents.contains(RESTART_INTERRUPTED_PROMPT));
        assert!(!contents.contains(RESTART_RESUMED_PROMPT));
    }
    for log in ["pmuxd.log", "pmuxd.log.previous"] {
        let path = sandbox.root_path.join("logs").join(log);
        if path.exists() {
            let contents = std::fs::read_to_string(path).unwrap();
            assert!(!contents.contains(RESTART_INTERRUPTED_PROMPT));
            assert!(!contents.contains(RESTART_RESUMED_PROMPT));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "launches exact candidate binaries, a private real rmux PTY, and a credential-free fake Claude"]
async fn all_v1_methods_use_the_real_public_and_private_process_boundaries() {
    let binaries = CandidateBinaries::from_environment();
    let client_assets = CrossClientAssets::from_workspace();
    let sandbox = Sandbox::new(&binaries);
    let mut daemon = DaemonGuard::start(&binaries, &sandbox).await;
    daemon.assert_identity(&binaries, &sandbox);
    daemon.assert_no_inet_sockets();
    let client = PmuxClient::new(&sandbox.public_socket).unwrap();

    assert_eq!(
        std::fs::metadata(&sandbox.public_socket)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    exercise_raw_transport(&sandbox.public_socket).await;
    exercise_public_connection_capacity(&sandbox.public_socket).await;

    let pong = client.ping().await.unwrap();
    assert_eq!(pong.protocol_version, 1);

    let launches_before_reserved_rejections = sandbox.launch_count();
    for (profile, input_transport) in [
        (TerminalProfile::RmuxStandard, InputTransport::Sdk),
        (TerminalProfile::Transparent, InputTransport::AttachedStream),
    ] {
        let mut request = sandbox.start_request(
            SessionIdentity::New {
                session_id: Some(Uuid::new_v4()),
            },
            RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000,
            },
        );
        request.terminal.profile = profile;
        request.terminal.input_transport = input_transport;
        let error = client.start_session(request).await.unwrap_err();
        assert_server_code(error, ErrorCode::UnsupportedFeature);
        assert_eq!(
            sandbox.launch_count(),
            launches_before_reserved_rejections,
            "a reserved terminal/input mode reached the Claude child boundary"
        );
    }

    let startup_modal_session_id = Uuid::new_v4();
    let mut startup_modal_request = sandbox.start_request(
        SessionIdentity::New {
            session_id: Some(startup_modal_session_id),
        },
        RetentionPolicy::Persistent {
            idle_ttl_ms: 60_000,
        },
    );
    startup_modal_request
        .environment
        .set
        .insert("PMUX_TEST_STARTUP_MODAL".into(), "permission".into());
    let startup_modal = client.start_session(startup_modal_request).await.unwrap();
    assert_eq!(
        startup_modal.state,
        pseudomux_protocol::v1::SessionState::NeedsInput
    );
    let startup_modal_launch = sandbox.only_launch_for_session(startup_modal_session_id);
    let startup_snapshot = client
        .inspect_session(startup_modal.session_id, startup_modal.generation_id)
        .await
        .unwrap();
    assert_eq!(
        startup_snapshot.needs_input.as_ref().unwrap().kind,
        pseudomux_protocol::v1::NeedsInputKind::Permission
    );
    let first_modal_attach = client
        .attach_session(AttachSessionRequest {
            session_id: startup_modal.session_id,
            generation_id: startup_modal.generation_id,
            read_only: false,
            size: None,
        })
        .await
        .unwrap();
    consume_attach_capability(&first_modal_attach.endpoint, &first_modal_attach.token).await;
    let second_modal_attach = wait_for_attach_reservation_release(&client, &startup_modal).await;
    consume_attach_capability(&second_modal_attach.endpoint, &second_modal_attach.token).await;
    let startup_modal_close = client
        .close_session(
            startup_modal.session_id,
            startup_modal.generation_id,
            ClosePolicy::Force,
        )
        .await
        .unwrap();
    assert!(startup_modal_close.process_reaped);
    assert_process_boundary_absent(
        &startup_modal_launch,
        "Claude process retained in startup permission needs-input",
    );

    let session_id = Uuid::new_v4();
    let first = client
        .start_session(sandbox.attested_start_request(
            SessionIdentity::New {
                session_id: Some(session_id),
            },
            RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000,
            },
        ))
        .await
        .unwrap();
    assert_eq!(first.session_id, session_id);
    assert!(first.compatibility.tested);
    let first_launch = sandbox.only_launch_for_session(session_id);
    let sensitive_launch_paths =
        sandbox.assert_rich_launch(&binaries, &first_launch, session_id, "new");

    let slow_public_clients = hold_slow_public_connections(&sandbox.public_socket).await;

    let sockets_before_read_only_attach = socket_identities_under(&sandbox.runtime_parent);
    let read_only = client
        .attach_session(AttachSessionRequest {
            session_id: first.session_id,
            generation_id: first.generation_id,
            read_only: true,
            size: None,
        })
        .await
        .unwrap_err();
    assert_server_code(read_only, ErrorCode::UnsupportedFeature);
    assert_eq!(
        socket_identities_under(&sandbox.runtime_parent),
        sockets_before_read_only_attach,
        "read-only rejection created a proxy endpoint or replaced a private socket"
    );

    for lease in [
        TurnLeasePolicy {
            on_disconnect: DisconnectAction::CancelTurn,
            heartbeat_timeout_ms: None,
        },
        TurnLeasePolicy {
            on_disconnect: DisconnectAction::Continue,
            heartbeat_timeout_ms: Some(1_000),
        },
    ] {
        let mut request = turn(Uuid::new_v4(), RESERVED_LEASE_PROMPT);
        request.lease = lease;
        let error = client
            .run_turn(first.session_id, first.generation_id, request)
            .await
            .unwrap_err();
        assert_server_code(error, ErrorCode::UnsupportedFeature);
    }

    let first_turn = submit_turn(&client, &first, FIRST_PROMPT).await;
    let first_result = wait_for_result(&client, &first, &first_turn, FIRST_PROMPT).await;
    assert_completed(&first_result, session_id, first_turn.turn_id);
    let first_transcript = sandbox
        .config_root
        .join("projects/pmux-e2e")
        .join(format!("{session_id}.jsonl"));
    assert!(
        !std::fs::read_to_string(first_transcript)
            .unwrap()
            .contains(RESERVED_LEASE_PROMPT),
        "a reserved connection lease crossed the public actor into Claude's transcript"
    );
    drop(slow_public_clients);
    assert_public_connection_recovered(&sandbox.public_socket).await;

    let snapshot = client
        .inspect_session(first.session_id, first.generation_id)
        .await
        .unwrap();
    assert_eq!(snapshot.last_turn.unwrap().turn_id, first_turn.turn_id);

    let rich_turn = submit_turn(&client, &first, RICH_RESULT_PROMPT).await;
    let rich_result = wait_for_result(&client, &first, &rich_turn, RICH_RESULT_PROMPT).await;
    assert_rich_result(&rich_result, session_id, rich_turn.turn_id);

    let capability = client
        .attach_session(AttachSessionRequest {
            session_id: first.session_id,
            generation_id: first.generation_id,
            read_only: false,
            size: None,
        })
        .await
        .unwrap();
    let endpoint_mode = std::fs::metadata(&capability.endpoint)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(endpoint_mode, 0o600);
    consume_attach_capability(&capability.endpoint, &capability.token).await;

    let cancelling = submit_turn(&client, &first, CANCEL_PROMPT).await;
    wait_for_prompt_ack(&client, &first, &cancelling).await;
    let cancelled = client
        .cancel_turn(first.session_id, first.generation_id, cancelling.turn_id)
        .await
        .unwrap();
    assert_eq!(cancelled.outcome, CancelOutcome::Cancelled);

    let after_cancel = submit_turn(&client, &first, AFTER_CANCEL_PROMPT).await;
    let after_cancel_result =
        wait_for_result(&client, &first, &after_cancel, AFTER_CANCEL_PROMPT).await;
    assert_completed(&after_cancel_result, session_id, after_cancel.turn_id);
    exercise_shipped_cli_attach(&binaries, &sandbox, &first).await;

    let closed = client
        .close_session(first.session_id, first.generation_id, ClosePolicy::Graceful)
        .await
        .unwrap();
    assert!(closed.process_reaped);
    assert_process_boundary_absent(&first_launch, "first persistent Claude process");
    for path in &sensitive_launch_paths {
        assert!(
            !path.exists(),
            "private launch material survived confirmed close: {}",
            path.display()
        );
    }

    let resumed = client
        .start_session(sandbox.start_request(
            SessionIdentity::Resume { session_id },
            RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000,
            },
        ))
        .await
        .unwrap();
    assert_ne!(resumed.generation_id, first.generation_id);

    let stale = client
        .inspect_session(first.session_id, first.generation_id)
        .await
        .unwrap_err();
    assert_server_code(stale, ErrorCode::StaleSessionGeneration);

    let stale_turn_id = Uuid::new_v4();
    let stale_turn = client
        .run_turn(
            first.session_id,
            first.generation_id,
            turn(stale_turn_id, "PMUX_TEST_STALE_MUST_NOT_REACH_RESUME"),
        )
        .await
        .unwrap_err();
    assert_server_code(stale_turn, ErrorCode::StaleSessionGeneration);
    let stale_cancel = client
        .cancel_turn(first.session_id, first.generation_id, stale_turn_id)
        .await
        .unwrap_err();
    assert_server_code(stale_cancel, ErrorCode::StaleSessionGeneration);
    let stale_attach = client
        .attach_session(AttachSessionRequest {
            session_id: first.session_id,
            generation_id: first.generation_id,
            read_only: false,
            size: None,
        })
        .await
        .unwrap_err();
    assert_server_code(stale_attach, ErrorCode::StaleSessionGeneration);
    let stale_subscribe = client
        .subscribe_events(SubscribeEventsRequest {
            session_id: first.session_id,
            generation_id: first.generation_id,
            after_sequence: 0,
            wait_ms: 0,
            max_events: 1,
        })
        .await
        .unwrap_err();
    assert_server_code(stale_subscribe, ErrorCode::StaleSessionGeneration);

    let old_close = client
        .close_session(first.session_id, first.generation_id, ClosePolicy::Force)
        .await
        .unwrap();
    assert!(old_close.already_closed);
    assert!(old_close.process_reaped);
    client
        .inspect_session(resumed.session_id, resumed.generation_id)
        .await
        .unwrap();

    let resumed_launches = sandbox.launches_for_session(session_id);
    assert_eq!(resumed_launches.len(), 2);
    sandbox.assert_plain_launch(
        &binaries,
        &resumed_launches[1],
        session_id,
        "resume",
        &["--model", "test-model", "--permission-mode", "default"],
    );

    let resumed_turn = submit_turn(&client, &resumed, RESUME_PROMPT).await;
    let resumed_result = wait_for_result(&client, &resumed, &resumed_turn, RESUME_PROMPT).await;
    assert_completed(&resumed_result, session_id, resumed_turn.turn_id);
    let resumed_close = client
        .close_session(
            resumed.session_id,
            resumed.generation_id,
            ClosePolicy::Graceful,
        )
        .await
        .unwrap();
    assert!(resumed_close.process_reaped);
    assert_process_boundary_absent(&resumed_launches[1], "resumed Claude process");

    let hybrid_session_id = Uuid::new_v4();
    let mut hybrid_request = sandbox.start_request(
        SessionIdentity::New {
            session_id: Some(hybrid_session_id),
        },
        RetentionPolicy::Persistent {
            idle_ttl_ms: 60_000,
        },
    );
    hybrid_request.lifecycle = LifecycleMode::Hybrid {
        hook_timeout_ms: 2_000,
    };
    let hybrid = client.start_session(hybrid_request).await.unwrap();
    let hybrid_launch = sandbox.only_launch_for_session(hybrid_session_id);
    let hybrid_settings =
        sandbox.assert_hybrid_launch(&binaries, &hybrid_launch, hybrid_session_id);
    let hybrid_turn = submit_turn(&client, &hybrid, HYBRID_PROMPT).await;
    let hybrid_result = wait_for_result(&client, &hybrid, &hybrid_turn, HYBRID_PROMPT).await;
    assert_completed(&hybrid_result, hybrid_session_id, hybrid_turn.turn_id);
    assert!(hybrid_result.completion.lifecycle_hook_observed);
    // The instant reaches the wire whenever the boolean does, which is what
    // makes the signed difference against `last_transcript_activity_at_ms`
    // collectable from a real end-to-end run.
    assert!(
        hybrid_result.timings.stop_hook_at_ms.is_some(),
        "an observed Stop hook must publish its instant"
    );
    assert!(
        hybrid_result
            .warnings
            .iter()
            .all(|warning| warning.code != "lifecycle_hook_missing")
    );
    let hybrid_close = client
        .close_session(
            hybrid.session_id,
            hybrid.generation_id,
            ClosePolicy::Graceful,
        )
        .await
        .unwrap();
    assert!(hybrid_close.process_reaped);
    assert!(!hybrid_settings.exists());
    assert_process_boundary_absent(&hybrid_launch, "Hybrid Claude process");
    let hook_invocations =
        std::fs::read_to_string(sandbox.state_root.join("hook-invocations.jsonl")).unwrap();
    let hook_events = hook_invocations
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()["event"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        hook_events,
        vec![serde_json::json!("SessionStart"), serde_json::json!("Stop")]
    );

    let ambiguous_paste_id = Uuid::new_v4();
    let ambiguous_paste = client
        .start_session(sandbox.start_request(
            SessionIdentity::New {
                session_id: Some(ambiguous_paste_id),
            },
            RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000,
            },
        ))
        .await
        .unwrap();
    let ambiguous_launch = sandbox.only_launch_for_session(ambiguous_paste_id);
    let ambiguous_identity =
        exact_process_identity_from_launch(&ambiguous_launch, &binaries.fake_claude);
    let mut ambiguous_request = turn(Uuid::new_v4(), AMBIGUOUS_PASTE_PROMPT);
    ambiguous_request.deadline_unix_ms = Some(now_ms() + 3_000);
    let ambiguous_turn = client
        .run_turn(
            ambiguous_paste.session_id,
            ambiguous_paste.generation_id,
            ambiguous_request,
        )
        .await
        .unwrap();
    let ambiguous_failure = wait_for_failure(&client, &ambiguous_paste, &ambiguous_turn).await;
    assert_eq!(ambiguous_failure.code, ErrorCode::TurnTimeout);
    assert!(!ambiguous_failure.retryable);
    assert!(
        !sandbox
            .state_root
            .join(format!("unexpected-input-{ambiguous_paste_id}.bin"))
            .exists(),
        "pmux sent Enter or another byte after the real PTY paste render remained ambiguous"
    );
    wait_for_exact_process_absence(&ambiguous_identity, Duration::from_secs(10)).await;
    assert_process_boundary_absent(
        &ambiguous_launch,
        "Claude process after a real PTY ambiguous paste",
    );
    let ambiguous_snapshot = client
        .inspect_session(ambiguous_paste.session_id, ambiguous_paste.generation_id)
        .await
        .unwrap();
    assert_eq!(
        ambiguous_snapshot.state,
        pseudomux_protocol::v1::SessionState::Failed
    );
    let ambiguous_close = client
        .close_session(
            ambiguous_paste.session_id,
            ambiguous_paste.generation_id,
            ClosePolicy::Force,
        )
        .await
        .unwrap();
    assert!(ambiguous_close.process_reaped);

    let modal_session_id = Uuid::new_v4();
    let modal_session = client
        .start_session(sandbox.start_request(
            SessionIdentity::New {
                session_id: Some(modal_session_id),
            },
            RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000,
            },
        ))
        .await
        .unwrap();
    let modal_launch = sandbox.only_launch_for_session(modal_session_id);
    let modal_turn = submit_turn(&client, &modal_session, ADMISSION_MODAL_PROMPT).await;
    let modal_failure = wait_for_failure(&client, &modal_session, &modal_turn).await;
    assert_eq!(modal_failure.code, ErrorCode::NeedsPermission);
    assert!(!modal_failure.retryable);
    let modal_snapshot = client
        .inspect_session(modal_session.session_id, modal_session.generation_id)
        .await
        .unwrap();
    assert_eq!(
        modal_snapshot.state,
        pseudomux_protocol::v1::SessionState::Failed
    );
    assert_eq!(
        modal_snapshot.last_turn.unwrap().turn_id,
        modal_turn.turn_id
    );
    assert_process_boundary_absent(
        &modal_launch,
        "Claude process that rendered a post-paste admission modal",
    );
    let modal_close = client
        .close_session(
            modal_session.session_id,
            modal_session.generation_id,
            ClosePolicy::Force,
        )
        .await
        .unwrap();
    assert!(modal_close.process_reaped);

    let post_enter_modal_id = Uuid::new_v4();
    let post_enter_modal = client
        .start_session(sandbox.start_request(
            SessionIdentity::New {
                session_id: Some(post_enter_modal_id),
            },
            RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000,
            },
        ))
        .await
        .unwrap();
    let post_enter_launch = sandbox.only_launch_for_session(post_enter_modal_id);
    let post_enter_turn = submit_turn(&client, &post_enter_modal, POST_ENTER_MODAL_PROMPT).await;
    let needs_input = wait_for_turn_needs_input(&client, &post_enter_modal).await;
    assert_eq!(
        needs_input.needs_input.as_ref().unwrap().kind,
        pseudomux_protocol::v1::NeedsInputKind::Permission
    );
    assert!(
        needs_input
            .last_turn
            .as_ref()
            .is_none_or(|turn| turn.turn_id != post_enter_turn.turn_id),
        "the blocked post-Enter turn committed before explicit input"
    );
    let post_enter_attach = client
        .attach_session(AttachSessionRequest {
            session_id: post_enter_modal.session_id,
            generation_id: post_enter_modal.generation_id,
            read_only: false,
            size: None,
        })
        .await
        .unwrap();
    answer_attach_capability(
        &post_enter_attach.endpoint,
        &post_enter_attach.token,
        b"y\r",
    )
    .await;
    let post_enter_result = wait_for_result(
        &client,
        &post_enter_modal,
        &post_enter_turn,
        POST_ENTER_MODAL_PROMPT,
    )
    .await;
    assert_completed(
        &post_enter_result,
        post_enter_modal_id,
        post_enter_turn.turn_id,
    );
    let post_enter_close = client
        .close_session(
            post_enter_modal.session_id,
            post_enter_modal.generation_id,
            ClosePolicy::Graceful,
        )
        .await
        .unwrap();
    assert!(post_enter_close.process_reaped);
    assert_process_boundary_absent(
        &post_enter_launch,
        "Claude process after explicit post-Enter permission resolution",
    );

    let one_shot_id = Uuid::new_v4();
    let one_shot_turn = Uuid::new_v4();
    let one_shot = client
        .run_once(RunOnceRequest {
            session: sandbox.start_request(
                SessionIdentity::New {
                    session_id: Some(one_shot_id),
                },
                RetentionPolicy::OneShot,
            ),
            turn: turn(one_shot_turn, RUN_ONCE_PROMPT),
        })
        .await
        .unwrap();
    assert_completed(&one_shot, one_shot_id, one_shot_turn);
    let one_shot_launch = sandbox.only_launch_for_session(one_shot_id);
    sandbox.assert_plain_launch(
        &binaries,
        &one_shot_launch,
        one_shot_id,
        "new",
        &["--model", "test-model", "--permission-mode", "default"],
    );
    assert_process_boundary_absent(&one_shot_launch, "native run_once Claude process");

    exercise_shipped_cli(&binaries, &sandbox).await;
    exercise_shipped_mcp(&binaries, &sandbox).await;
    exercise_shipped_facade(&binaries, &sandbox);
    exercise_external_client(
        &binaries,
        &client_assets,
        &sandbox,
        ClientLanguage::TypeScript,
    )
    .await;
    exercise_external_client(&binaries, &client_assets, &sandbox, ClientLanguage::Python).await;
    exercise_actual_binary_resource_soak(&binaries, &client, &sandbox, daemon.pid()).await;

    let shutdown_session_id = Uuid::new_v4();
    let shutdown_session = client
        .start_session(sandbox.start_request(
            SessionIdentity::New {
                session_id: Some(shutdown_session_id),
            },
            RetentionPolicy::Persistent {
                idle_ttl_ms: 60_000,
            },
        ))
        .await
        .unwrap();
    let shutdown_launch = sandbox.only_launch_for_session(shutdown_session_id);
    let shutdown_turn = submit_turn(&client, &shutdown_session, CANCEL_PROMPT).await;
    wait_for_prompt_ack(&client, &shutdown_session, &shutdown_turn).await;

    let attach_token = capability.token;
    daemon.assert_identity(&binaries, &sandbox);
    daemon.assert_no_inet_sockets();
    daemon.stop().await;
    assert_process_boundary_absent(
        &shutdown_launch,
        "Claude process active during graceful daemon shutdown",
    );
    binaries.assert_unchanged();
    client_assets.assert_unchanged();
    sandbox.assert_clean(&binaries, &attach_token);
}

async fn exercise_public_connection_capacity(socket: &Path) {
    const CONNECTIONS: usize = 64;
    let barrier = Arc::new(Barrier::new(CONNECTIONS + 1));
    let mut clients = JoinSet::new();
    for index in 0..CONNECTIONS {
        let socket = socket.to_path_buf();
        let barrier = Arc::clone(&barrier);
        clients.spawn(async move {
            let request_id = Uuid::new_v4();
            let mut stream = UnixStream::connect(socket).await.unwrap();
            barrier.wait().await;
            let payload = if index % 4 == 0 {
                serde_json::to_vec(&serde_json::json!({
                    "version": 1,
                    "request_id": request_id,
                    "method": "ping",
                    "unknown_execution_field": index,
                }))
                .unwrap()
            } else {
                serde_json::to_vec(&RequestEnvelope::new(request_id, Request::Ping)).unwrap()
            };
            write_native_frame(&mut stream, &payload).await;
            let response = read_native_response(&mut stream).await;
            assert_eq!(response.request_id, request_id);
            if index % 4 == 0 {
                assert!(matches!(
                    response.payload,
                    ResponsePayload::Failure(error) if error.code == ErrorCode::InvalidConfig
                ));
            } else {
                assert!(matches!(
                    response.payload,
                    ResponsePayload::Success(result)
                        if matches!(*result, pseudomux_protocol::v1::ResponseResult::Pong(_))
                ));
            }
        });
    }
    barrier.wait().await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(result) = clients.join_next().await {
            result.unwrap();
        }
    })
    .await
    .expect("64 public mixed-client connections did not recover within the bound");

    assert_public_connection_recovered(socket).await;
}

async fn hold_slow_public_connections(socket: &Path) -> Vec<UnixStream> {
    const HELD_CONNECTIONS: usize = 63;
    const UNREAD_RESPONSES: usize = 16;
    const PARTIAL_REQUESTS: usize = 16;

    let mut held = Vec::with_capacity(HELD_CONNECTIONS);
    for _ in 0..HELD_CONNECTIONS {
        let request_id = Uuid::new_v4();
        let mut stream = UnixStream::connect(socket).await.unwrap();
        write_native_frame(
            &mut stream,
            &serde_json::to_vec(&RequestEnvelope::new(request_id, Request::Ping)).unwrap(),
        )
        .await;
        assert_eq!(
            read_native_response(&mut stream).await.request_id,
            request_id
        );
        held.push(stream);
    }

    // These already-admitted clients now stop consuming a second valid
    // response, while a disjoint set stops midway through the next frame
    // header. The remaining clients are idle between complete frames. All 63
    // therefore retain real daemon permits through three distinct slow-client
    // states while the single remaining slot services an unrelated session.
    for stream in held.iter_mut().take(UNREAD_RESPONSES) {
        let request = RequestEnvelope::new(Uuid::new_v4(), Request::Ping);
        write_native_frame(stream, &serde_json::to_vec(&request).unwrap()).await;
    }
    for stream in held
        .iter_mut()
        .skip(UNREAD_RESPONSES)
        .take(PARTIAL_REQUESTS)
    {
        stream.write_all(&[0, 0]).await.unwrap();
        stream.flush().await.unwrap();
    }
    tokio::task::yield_now().await;
    held
}

async fn assert_public_connection_recovered(socket: &Path) {
    let mut recovered = UnixStream::connect(socket).await.unwrap();
    let request_id = Uuid::new_v4();
    write_native_frame(
        &mut recovered,
        &serde_json::to_vec(&RequestEnvelope::new(request_id, Request::Ping)).unwrap(),
    )
    .await;
    assert_eq!(
        read_native_response(&mut recovered).await.request_id,
        request_id
    );
}

async fn exercise_actual_binary_resource_soak(
    binaries: &CandidateBinaries,
    client: &PmuxClient,
    sandbox: &Sandbox,
    daemon_pid: u32,
) {
    const ITERATIONS: usize = 24;
    const MAX_RETAINED_RSS_GROWTH_KIB: u64 = 64 * 1024;
    const MAX_DESCRIPTOR_GROWTH: usize = 8;
    const MAX_DAEMON_LOG_BYTES: u64 = 16 * 1024 * 1024;

    let baseline = process_resources(daemon_pid);
    let baseline_runtime = runtime_entries(&sandbox.runtime_parent);
    let mut midpoint = None;
    for index in 0..ITERATIONS {
        let session_id = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        let result = client
            .run_once(RunOnceRequest {
                session: sandbox.start_request(
                    SessionIdentity::New {
                        session_id: Some(session_id),
                    },
                    RetentionPolicy::OneShot,
                ),
                turn: turn(turn_id, &format!("PMUX_TEST_RESOURCE_SOAK_{index}")),
            })
            .await
            .unwrap();
        assert_completed(&result, session_id, turn_id);
        let inspect = client
            .inspect_session(session_id, result.generation_id)
            .await
            .expect_err("one-shot resource iteration retained an actor");
        assert_server_code(inspect, ErrorCode::SessionNotFound);
        let launch = sandbox.only_launch_for_session(session_id);
        sandbox.assert_plain_launch(
            binaries,
            &launch,
            session_id,
            "new",
            &["--model", "test-model", "--permission-mode", "default"],
        );
        assert_process_boundary_absent(&launch, "resource-soak Claude process");
        if index + 1 == ITERATIONS / 2 {
            midpoint = Some(process_resources(daemon_pid));
        }
    }

    let midpoint = midpoint.unwrap();
    let after = process_resources(daemon_pid);
    for (label, observation) in [("midpoint", midpoint), ("final", after)] {
        assert!(
            observation.rss_kib <= baseline.rss_kib + MAX_RETAINED_RSS_GROWTH_KIB,
            "pmuxd RSS grew beyond the deterministic soak ceiling at {label}: baseline={}KiB observed={}KiB",
            baseline.rss_kib,
            observation.rss_kib
        );
        assert!(
            observation.open_fds <= baseline.open_fds + MAX_DESCRIPTOR_GROWTH,
            "pmuxd descriptor count grew beyond the soak ceiling at {label}: baseline={} observed={}",
            baseline.open_fds,
            observation.open_fds
        );
    }
    assert_eq!(
        runtime_entries(&sandbox.runtime_parent),
        baseline_runtime,
        "one-shot resource soak left private runtime artifacts"
    );
    assert!(
        std::fs::metadata(sandbox.root_path.join("logs/pmuxd.log"))
            .unwrap()
            .len()
            <= MAX_DAEMON_LOG_BYTES,
        "pmuxd log exceeded its production byte ceiling"
    );
}

async fn exercise_raw_transport(socket: &Path) {
    let first_id = Uuid::new_v4();
    let mut stream = UnixStream::connect(socket).await.unwrap();
    write_native_frame(
        &mut stream,
        &serde_json::to_vec(&RequestEnvelope::new(first_id, Request::Ping)).unwrap(),
    )
    .await;
    let response = read_native_response(&mut stream).await;
    assert_eq!(response.request_id, first_id);
    assert!(matches!(
        response.payload,
        ResponsePayload::Success(result)
            if matches!(*result, pseudomux_protocol::v1::ResponseResult::Pong(_))
    ));

    // A strict envelope error is correlated and does not desynchronize a
    // reusable connection. This is the actual daemon process, not the handler
    // helper used by unit tests.
    let malformed_id = Uuid::new_v4();
    let malformed = serde_json::json!({
        "version": 1,
        "request_id": malformed_id,
        "method": "ping",
        "unknown_execution_field": true,
    });
    write_native_frame(&mut stream, &serde_json::to_vec(&malformed).unwrap()).await;
    let response = read_native_response(&mut stream).await;
    assert_eq!(response.request_id, malformed_id);
    assert!(matches!(
        response.payload,
        ResponsePayload::Failure(error) if error.code == ErrorCode::InvalidConfig
    ));

    let recovered_id = Uuid::new_v4();
    write_native_frame(
        &mut stream,
        &serde_json::to_vec(&RequestEnvelope::new(recovered_id, Request::Ping)).unwrap(),
    )
    .await;
    assert_eq!(
        read_native_response(&mut stream).await.request_id,
        recovered_id
    );

    // The unread oversized body is rejected from its header and the connection
    // is then closed because it cannot be safely resynchronized.
    let mut oversized = UnixStream::connect(socket).await.unwrap();
    oversized
        .write_all(
            &u32::try_from(MAX_NATIVE_FRAME_BYTES + 1)
                .unwrap()
                .to_be_bytes(),
        )
        .await
        .unwrap();
    let response = read_native_response(&mut oversized).await;
    assert!(matches!(
        response.payload,
        ResponsePayload::Failure(error) if error.code == ErrorCode::InvalidConfig
    ));
    let mut byte = [0_u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(2), oversized.read(&mut byte))
        .await
        .expect("oversized native connection did not close")
        .unwrap();
    assert_eq!(read, 0);
}

async fn write_native_frame(stream: &mut UnixStream, payload: &[u8]) {
    stream
        .write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())
        .await
        .unwrap();
    stream.write_all(payload).await.unwrap();
    stream.flush().await.unwrap();
}

async fn read_native_response(stream: &mut UnixStream) -> ResponseEnvelope {
    let mut header = [0_u8; 4];
    tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut header))
        .await
        .expect("native response header timed out")
        .unwrap();
    let length = u32::from_be_bytes(header) as usize;
    assert!(length <= MAX_NATIVE_FRAME_BYTES);
    let mut payload = vec![0_u8; length];
    tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut payload))
        .await
        .expect("native response body timed out")
        .unwrap();
    serde_json::from_slice(&payload).unwrap()
}

async fn exercise_shipped_cli(binaries: &CandidateBinaries, sandbox: &Sandbox) {
    let cli_session_id = Uuid::new_v4();
    let mut command = Command::new(&binaries.pmux);
    command
        .arg("--socket")
        .arg(&sandbox.public_socket)
        .arg("--output")
        .arg("ndjson")
        .arg("oneshot")
        .arg("--claude")
        .arg(&sandbox.fake_claude)
        .arg("--cwd")
        .arg(&sandbox.cwd)
        .arg("--session-id")
        .arg(cli_session_id.to_string())
        .arg("--model")
        .arg("test-model")
        .arg("--input-transport")
        .arg("sdk")
        .arg("--timeout-secs")
        .arg("30")
        .arg(CLI_PROMPT);
    sandbox.configure_external_environment(&mut command);
    let output = run_bounded(command, Duration::from_secs(40));
    assert_success(&output, "pmux CLI run");
    assert_output_does_not_leak(&output, CLI_PROMPT);
    let records = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(!records.is_empty());
    assert_eq!(records.last().unwrap()["type"], "result");
    assert_eq!(records.last().unwrap()["data"]["text"], "pmux-test-ok");
    assert_eq!(
        records.last().unwrap()["data"]["session_id"],
        cli_session_id.to_string()
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record["type"] == "result")
            .count(),
        1
    );

    let final_result = &records.last().unwrap()["data"];
    let generation_id = serde_json::from_value(final_result["generation_id"].clone()).unwrap();
    let client = PmuxClient::new(&sandbox.public_socket).unwrap();
    let inspect = client
        .inspect_session(cli_session_id, generation_id)
        .await
        .expect_err("a successful one-shot CLI run must unregister its actor before committing");
    assert_server_code(inspect, ErrorCode::SessionNotFound);

    let launch = sandbox.only_launch_for_session(cli_session_id);
    sandbox.assert_plain_launch(
        binaries,
        &launch,
        cli_session_id,
        "new",
        &["--model", "test-model"],
    );
    assert_process_boundary_absent(&launch, "pmux CLI one-shot Claude process");
}

async fn exercise_shipped_cli_attach(
    binaries: &CandidateBinaries,
    sandbox: &Sandbox,
    handle: &SessionHandle,
) {
    const ROWS: u16 = 39;
    const COLS: u16 = 113;

    let sockets_before = socket_identities_under(&sandbox.runtime_parent);
    assert_eq!(sockets_before.len(), 2);
    let (master, slave) = open_test_pty(ROWS, COLS);
    let observer = slave.try_clone().unwrap();
    let original_termios = termios_snapshot(&observer);
    assert_ne!(original_termios.local_flags & libc::ECHO, 0);
    assert_ne!(original_termios.local_flags & libc::ICANON, 0);

    let mut command = Command::new(&binaries.pmux);
    command
        .arg("--socket")
        .arg(&sandbox.public_socket)
        .arg("--output")
        .arg("text")
        .arg("attach")
        .arg(handle.session_id.to_string())
        .arg("--generation")
        .arg(handle.generation_id.to_string())
        .arg("--rows")
        .arg(ROWS.to_string())
        .arg("--cols")
        .arg(COLS.to_string())
        .env_clear()
        .env("TERM", "xterm-256color")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    drop(slave);

    let raw_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let active = termios_snapshot(&observer);
        if active.local_flags & (libc::ECHO | libc::ICANON) == 0 {
            break;
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("exact release pmux attach exited before entering raw mode: {status}");
        }
        assert!(
            std::time::Instant::now() < raw_deadline,
            "exact release pmux attach did not enter raw mode"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let pid = child.id();
    // SAFETY: this targets only the positive PID retained from the exact
    // release-candidate CLI child owned by this test. The same handle is
    // subsequently waited and its start cannot alias before that wait.
    let sent = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    assert_eq!(
        sent,
        0,
        "failed to signal exact release pmux attach child {pid}: {}",
        std::io::Error::last_os_error()
    );
    let output = wait_bounded(child, Duration::from_secs(5));
    assert_eq!(output.status.signal(), Some(libc::SIGTERM));
    assert_output_does_not_leak(&output, FIRST_PROMPT);

    let restored = termios_snapshot(&observer);
    assert_terminal_restored(&restored, &original_termios);
    drop(observer);
    drop(master);

    let client = PmuxClient::new(&sandbox.public_socket).unwrap();
    for _ in 0..200 {
        let snapshot = client
            .inspect_session(handle.session_id, handle.generation_id)
            .await
            .unwrap();
        if snapshot.state == pseudomux_protocol::v1::SessionState::Ready {
            assert_eq!(
                socket_identities_under(&sandbox.runtime_parent),
                sockets_before,
                "release CLI attach left a proxy endpoint or replaced a private socket"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("session did not reconcile after exact release CLI attach termination");
}

async fn exercise_shipped_mcp(binaries: &CandidateBinaries, sandbox: &Sandbox) {
    let session_id = Uuid::new_v4();
    let turn_id = Uuid::new_v4();
    let request = RunOnceRequest {
        session: sandbox.start_request(
            SessionIdentity::New {
                session_id: Some(session_id),
            },
            RetentionPolicy::OneShot,
        ),
        turn: turn(turn_id, MCP_PROMPT),
    };
    let input = format!(
        "{}\n{}\n",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "pmux-e2e", "version": "1"}
            }
        }),
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "run_once",
                "arguments": request
            }
        })
    );
    let mut command = Command::new(&binaries.mcp);
    command
        .arg("--socket")
        .arg(&sandbox.public_socket)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = run_bounded_with_stdin(command, input.as_bytes(), Duration::from_secs(40));
    assert_success(&output, "pmux MCP run_once");
    assert_output_does_not_leak(&output, MCP_PROMPT);
    let responses = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    let result = &responses[1]["result"];
    assert!(result["content"].as_array().is_some_and(Vec::is_empty));
    assert_eq!(result["structuredContent"]["text"], "pmux-test-ok");
    assert_eq!(result["isError"], false);
    assert_eq!(
        result["structuredContent"]["session_id"],
        session_id.to_string()
    );
    assert_eq!(result["structuredContent"]["turn_id"], turn_id.to_string());
    let generation_id =
        serde_json::from_value(result["structuredContent"]["generation_id"].clone()).unwrap();
    let client = PmuxClient::new(&sandbox.public_socket).unwrap();
    let inspect = client
        .inspect_session(session_id, generation_id)
        .await
        .expect_err("MCP run_once retained its one-shot actor after returning success");
    assert_server_code(inspect, ErrorCode::SessionNotFound);
    let launch = sandbox.only_launch_for_session(session_id);
    sandbox.assert_plain_launch(
        binaries,
        &launch,
        session_id,
        "new",
        &["--model", "test-model", "--permission-mode", "default"],
    );
    assert_process_boundary_absent(&launch, "MCP run_once Claude process");
}

fn exercise_shipped_facade(binaries: &CandidateBinaries, sandbox: &Sandbox) {
    let session_id = Uuid::new_v4();
    let mut command = Command::new(&binaries.claude_p);
    command
        .arg("--socket")
        .arg(&sandbox.public_socket)
        .arg("--claude-bin")
        .arg(&sandbox.fake_claude)
        .arg("--cwd")
        .arg(&sandbox.cwd)
        .arg("--session-id")
        .arg(session_id.to_string())
        .arg("--output-format")
        .arg("stream-json")
        .arg("--timeout-seconds")
        .arg("30")
        .arg(FACADE_PROMPT);
    sandbox.configure_external_environment(&mut command);
    let output = run_bounded(command, Duration::from_secs(40));
    assert_success(&output, "claude-p facade run_once");
    assert_output_does_not_leak(&output, FACADE_PROMPT);
    let records = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["type"], "system");
    assert_eq!(
        records[0]["provenance"],
        "pmux_interactive_transcript_reconstruction"
    );
    assert_eq!(records[1]["type"], "assistant");
    assert_eq!(records.last().unwrap()["type"], "result");
    assert_eq!(records.last().unwrap()["subtype"], "success");
    assert_eq!(records.last().unwrap()["result"], "pmux-test-ok");
    assert_eq!(
        records.last().unwrap()["provenance"],
        "pmux_interactive_transcript_reconstruction"
    );
    let launch = sandbox.only_launch_for_session(session_id);
    sandbox.assert_plain_launch(binaries, &launch, session_id, "new", &[]);
    assert_process_boundary_absent(&launch, "claude-p run_once Claude process");

    exercise_shipped_facade_in_the_campaign_shape(binaries, sandbox);
}

/// The shape `tools/phase0/phase0_lib.py:5658-5703` will spend a live ordinal
/// on: `-p` first, the prompt on **stdin**, and `--output-format json`. The leg
/// above uses a positional prompt and `stream-json`, so the argv a campaign
/// actually runs had never been executed against a real daemon and a real PTY.
/// `PMUX_TEST_ECHO:` makes the assertion end-to-end: the bytes written to the
/// facade's stdin are the bytes that reached Claude.
fn exercise_shipped_facade_in_the_campaign_shape(binaries: &CandidateBinaries, sandbox: &Sandbox) {
    const MARKER: &str = "facade-stdin-json";
    let prompt = format!("PMUX_TEST_ECHO:{MARKER}");

    let session_id = Uuid::new_v4();
    let mut command = Command::new(&binaries.claude_p);
    command
        .arg("-p")
        .arg("--socket")
        .arg(&sandbox.public_socket)
        .arg("--claude-bin")
        .arg(&sandbox.fake_claude)
        .arg("--cwd")
        .arg(&sandbox.cwd)
        .arg("--session-id")
        .arg(session_id.to_string())
        .arg("--output-format")
        .arg("json")
        .arg("--timeout-seconds")
        .arg("30");
    sandbox.configure_external_environment(&mut command);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = run_bounded_with_stdin(command, prompt.as_bytes(), Duration::from_secs(40));
    assert_success(&output, "claude-p facade stdin run_once");

    // `assert_output_does_not_leak` is not reusable here: this prompt asks the
    // test double to echo its own payload, so the payload is legitimately in
    // stdout. The secrets must still be absent.
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains(TEST_ANTHROPIC_SECRET));
        assert!(!text.contains(TEST_PROVIDER_SECRET));
        assert!(!text.contains(TEST_LAUNCH_SECRET));
    }

    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["outcome"], "completed");
    assert_eq!(result["text"], format!("pmux-test-echo:{MARKER}"));
    assert_eq!(result["session_id"], session_id.to_string());
    assert_eq!(result["completion"]["authority"], "transcript");
    assert_eq!(result["completion"]["transcript_drained"], true);
    assert!(result["turn_id"].as_str().is_some());

    let launch = sandbox.only_launch_for_session(session_id);
    sandbox.assert_plain_launch(binaries, &launch, session_id, "new", &[]);
    assert_process_boundary_absent(&launch, "claude-p stdin run_once Claude process");
}

#[derive(Clone, Copy, Debug)]
enum ClientLanguage {
    TypeScript,
    Python,
}

impl ClientLanguage {
    fn label(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::Python => "python",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::TypeScript => "TypeScript",
            Self::Python => "Python",
        }
    }

    fn prompts(self) -> [&'static str; 5] {
        match self {
            Self::TypeScript => [
                "PMUX_TEST_TYPESCRIPT_FIRST",
                "PMUX_TEST_CANCEL_TYPESCRIPT_HOLD",
                "PMUX_TEST_TYPESCRIPT_AFTER_CANCEL",
                "PMUX_TEST_TYPESCRIPT_RESUMED",
                "PMUX_TEST_TYPESCRIPT_RUN_ONCE",
            ],
            Self::Python => [
                "PMUX_TEST_PYTHON_FIRST",
                "PMUX_TEST_CANCEL_PYTHON_HOLD",
                "PMUX_TEST_PYTHON_AFTER_CANCEL",
                "PMUX_TEST_PYTHON_RESUMED",
                "PMUX_TEST_PYTHON_RUN_ONCE",
            ],
        }
    }
}

struct ClientScenario {
    config_path: PathBuf,
    persistent_session: Uuid,
    first_turn: Uuid,
    cancel_turn: Uuid,
    recovery_turn: Uuid,
    resumed_turn: Uuid,
    once_session: Uuid,
    once_turn: Uuid,
    prompts: [&'static str; 5],
}

struct CrossClientAssets {
    typescript_root: PathBuf,
    typescript_dist_root: PathBuf,
    python_root: PathBuf,
    node: PathBuf,
    python: PathBuf,
    node_version: String,
    python_version: String,
    identities: BTreeMap<PathBuf, ExecutableIdentity>,
}

impl CrossClientAssets {
    fn from_workspace() -> Self {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()
            .expect("the E2E crate source directory must exist");
        let workspace = manifest_dir
            .parent()
            .and_then(Path::parent)
            .expect("the E2E crate must be nested below the workspace")
            .canonicalize()
            .unwrap();
        let typescript_root = required_source_directory(&workspace, "clients/typescript");
        let typescript_dist_root =
            required_external_directory(&workspace, "PMUX_E2E_TYPESCRIPT_DIST_DIR");
        validate_typescript_dist_root(&typescript_dist_root);
        let python_root = required_source_directory(&workspace, "clients/python");
        let node = required_runtime("PMUX_E2E_NODE", "node", ClientLanguage::TypeScript);
        let python = required_runtime("PMUX_E2E_PYTHON", "python3", ClientLanguage::Python);
        let node_version = exact_runtime_version(&node, false);
        let python_version = exact_runtime_version(&python, true);

        let mut paths = Vec::new();
        paths.extend(TYPESCRIPT_CLIENT_ASSETS.iter().map(|relative| {
            typescript_asset_path(&typescript_root, &typescript_dist_root, relative)
        }));
        paths.extend(
            PYTHON_CLIENT_ASSETS
                .iter()
                .map(|relative| required_source_file(&python_root, relative)),
        );
        paths.extend([node.clone(), python.clone()]);
        let identities = paths
            .into_iter()
            .map(|path| {
                let identity = executable_identity(&path);
                (path, identity)
            })
            .collect();

        Self {
            typescript_root,
            typescript_dist_root,
            python_root,
            node,
            python,
            node_version,
            python_version,
            identities,
        }
    }

    fn root(&self, language: ClientLanguage) -> &Path {
        match language {
            ClientLanguage::TypeScript => &self.typescript_root,
            ClientLanguage::Python => &self.python_root,
        }
    }

    fn helper(&self, language: ClientLanguage) -> PathBuf {
        match language {
            ClientLanguage::TypeScript => self.typescript_root.join("tests/actual_daemon_e2e.mjs"),
            ClientLanguage::Python => self.python_root.join("tests/actual_daemon_e2e.py"),
        }
    }

    fn entry(&self, language: ClientLanguage) -> PathBuf {
        match language {
            ClientLanguage::TypeScript => self.typescript_dist_root.join("index.js"),
            ClientLanguage::Python => self.python_root.join("pmux_client/__init__.py"),
        }
    }

    fn asset(&self, language: ClientLanguage, relative: &str) -> PathBuf {
        match language {
            ClientLanguage::TypeScript => {
                typescript_asset_path(&self.typescript_root, &self.typescript_dist_root, relative)
            }
            ClientLanguage::Python => required_source_file(&self.python_root, relative),
        }
    }

    fn runtime(&self, language: ClientLanguage) -> &Path {
        match language {
            ClientLanguage::TypeScript => &self.node,
            ClientLanguage::Python => &self.python,
        }
    }

    fn runtime_version(&self, language: ClientLanguage) -> &str {
        match language {
            ClientLanguage::TypeScript => &self.node_version,
            ClientLanguage::Python => &self.python_version,
        }
    }

    fn manifest(&self, language: ClientLanguage) -> &'static [&'static str] {
        match language {
            ClientLanguage::TypeScript => TYPESCRIPT_CLIENT_ASSETS,
            ClientLanguage::Python => PYTHON_CLIENT_ASSETS,
        }
    }

    fn digest_hex(&self, path: &Path) -> String {
        let digest = self
            .identities
            .get(path)
            .unwrap_or_else(|| panic!("unbound cross-client asset: {}", path.display()))
            .sha256;
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn typescript_dist_sha256(&self) -> String {
        let mut encoded = String::from("{\"schema_version\":1,\"files\":[");
        let mut first = true;
        for relative in TYPESCRIPT_CLIENT_ASSETS
            .iter()
            .filter_map(|relative| relative.strip_prefix("dist/"))
        {
            if !first {
                encoded.push(',');
            }
            first = false;
            let path = self.typescript_dist_root.join(relative);
            encoded.push_str(&format!(
                "{{\"relative_path\":\"{relative}\",\"sha256\":\"{}\"}}",
                self.digest_hex(&path)
            ));
        }
        encoded.push_str("]}");
        let mut hasher = Sha256::new();
        hasher.update(b"pmux-typescript-dist-stage-v1\0");
        hasher.update(encoded.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    fn assert_unchanged(&self) {
        validate_typescript_dist_root(&self.typescript_dist_root);
        for (path, expected) in &self.identities {
            assert_eq!(
                &executable_identity(path),
                expected,
                "cross-client runtime/source changed during E2E: {}",
                path.display()
            );
        }
    }
}

fn required_source_directory(workspace: &Path, relative: &str) -> PathBuf {
    let path = workspace.join(relative).canonicalize().unwrap();
    assert_eq!(path.strip_prefix(workspace).unwrap(), Path::new(relative));
    assert!(
        path.is_dir(),
        "required client source root is not a directory"
    );
    path
}

fn required_external_directory(workspace: &Path, variable: &str) -> PathBuf {
    let supplied = PathBuf::from(
        std::env::var_os(variable)
            .unwrap_or_else(|| panic!("{variable} is required for cross-client E2E")),
    );
    validate_external_directory(workspace, variable, &supplied)
}

fn validate_external_directory(workspace: &Path, label: &str, supplied: &Path) -> PathBuf {
    assert!(supplied.is_absolute(), "{label} must be absolute");
    let supplied_metadata = std::fs::symlink_metadata(supplied)
        .unwrap_or_else(|error| panic!("{label} must exist: {error}"));
    assert!(
        !supplied_metadata.file_type().is_symlink() && supplied_metadata.is_dir(),
        "{label} must be a real directory"
    );
    let canonical = supplied
        .canonicalize()
        .unwrap_or_else(|error| panic!("{label} must exist: {error}"));
    assert_eq!(
        supplied, canonical,
        "{label} must name its canonical directory"
    );
    assert!(
        !canonical.starts_with(workspace),
        "{label} must be outside the canonical workspace"
    );
    assert_eq!(
        supplied_metadata.mode() & 0o777,
        0o700,
        "{label} must be owner-private"
    );
    canonical
}

fn typescript_asset_path(source_root: &Path, dist_root: &Path, relative: &str) -> PathBuf {
    relative.strip_prefix("dist/").map_or_else(
        || required_source_file(source_root, relative),
        |dist_relative| required_typescript_dist_file(dist_root, dist_relative),
    )
}

fn validate_typescript_dist_root(root: &Path) {
    let expected = TYPESCRIPT_CLIENT_ASSETS
        .iter()
        .filter_map(|relative| relative.strip_prefix("dist/"))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let actual = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .file_name()
                .into_string()
                .expect("TypeScript dist names must be UTF-8")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected, "TypeScript dist membership changed");

    let mut identities = BTreeSet::new();
    for relative in &expected {
        let path = required_typescript_dist_file(root, relative);
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(
            identities.insert((metadata.dev(), metadata.ino())),
            "TypeScript dist file aliases another stage member: {relative}"
        );
    }
    assert_eq!(
        std::fs::read(root.join("package.json")).unwrap(),
        TYPESCRIPT_DIST_PACKAGE,
        "TypeScript dist package scope changed"
    );
}

fn required_typescript_dist_file(root: &Path, relative: &str) -> PathBuf {
    let unresolved = root.join(relative);
    let metadata = std::fs::symlink_metadata(&unresolved).unwrap_or_else(|error| {
        panic!(
            "required TypeScript dist asset is missing ({}): {error}",
            unresolved.display()
        )
    });
    assert!(
        !metadata.file_type().is_symlink() && metadata.is_file(),
        "TypeScript dist asset must be a regular file: {}",
        unresolved.display()
    );
    assert_eq!(metadata.nlink(), 1, "TypeScript dist asset is hard-linked");
    assert_eq!(
        metadata.mode() & 0o777,
        0o600,
        "TypeScript dist asset must be owner-private"
    );
    let path = unresolved.canonicalize().unwrap();
    assert_eq!(path.parent(), Some(root));
    path
}

fn required_source_file(root: &Path, relative: &str) -> PathBuf {
    let path = root.join(relative).canonicalize().unwrap_or_else(|error| {
        panic!(
            "required cross-client source is missing ({}): {error}",
            root.join(relative).display()
        )
    });
    assert_eq!(
        path.strip_prefix(root).unwrap(),
        Path::new(relative),
        "cross-client asset resolved to an unexpected source path"
    );
    assert!(path.is_file(), "cross-client asset is not a regular file");
    path
}

fn required_runtime(variable: &str, fallback: &str, language: ClientLanguage) -> PathBuf {
    let selected = if let Some(value) = std::env::var_os(variable) {
        let path = PathBuf::from(value);
        assert!(path.is_absolute(), "{variable} must be an absolute path");
        path
    } else {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
            .map(|directory| directory.join(fallback))
            .find(|candidate| {
                std::fs::metadata(candidate).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .unwrap_or_else(|| panic!("{fallback} is required for cross-client E2E"))
    }
    .canonicalize()
    .unwrap();
    let metadata = std::fs::metadata(&selected).unwrap();
    assert!(metadata.is_file());
    assert_ne!(metadata.permissions().mode() & 0o111, 0);
    effective_runtime_path(&selected, language)
}

fn effective_runtime_path(selected: &Path, language: ClientLanguage) -> PathBuf {
    let mut command = Command::new(selected);
    match language {
        ClientLanguage::TypeScript => {
            command.args(["-p", "require('node:fs').realpathSync(process.execPath)"]);
        }
        ClientLanguage::Python => {
            command.args([
                "-c",
                "import os, sys; print(os.path.realpath(sys.executable))",
            ]);
        }
    }
    let output = command
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "failed to resolve the effective {} runtime behind {}: {error}",
                language.title(),
                selected.display()
            )
        });
    assert!(
        output.status.success(),
        "effective {} runtime query failed for {}: {}",
        language.title(),
        selected.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "effective {} runtime query emitted stderr: {}",
        language.title(),
        String::from_utf8_lossy(&output.stderr)
    );
    let reported = String::from_utf8(output.stdout).unwrap();
    let reported = reported.trim();
    assert!(!reported.is_empty());
    assert_eq!(reported.lines().count(), 1);
    let effective = PathBuf::from(reported);
    assert!(
        effective.is_absolute(),
        "effective {} runtime path must be absolute",
        language.title()
    );
    let effective = effective.canonicalize().unwrap();
    let metadata = std::fs::metadata(&effective).unwrap();
    assert!(metadata.is_file());
    assert_ne!(metadata.permissions().mode() & 0o111, 0);
    effective
}

#[test]
fn runtime_identity_resolves_the_effective_interpreter_behind_a_launcher_shim() {
    let actual = required_runtime(
        "PMUX_E2E_REGRESSION_PYTHON",
        "python3",
        ClientLanguage::Python,
    );
    let temporary = TempDir::new().unwrap();
    let wrapper = temporary.path().join("python-launcher-shim");
    let escaped = actual.to_string_lossy().replace('\'', "'\"'\"'");
    std::fs::write(&wrapper, format!("#!/bin/sh\nexec '{escaped}' \"$@\"\n")).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();

    assert_ne!(wrapper.canonicalize().unwrap(), actual);
    assert_eq!(
        effective_runtime_path(&wrapper, ClientLanguage::Python),
        actual
    );
}

#[test]
fn result_observer_budget_is_the_semantic_deadline_plus_fixed_grace() {
    assert_eq!(FAKE_TURN_DEADLINE_MS, 30_000);
    assert_eq!(RESULT_OBSERVER_GRACE_MS, 10_000);
    assert_eq!(RESULT_OBSERVER_BUDGET_MS, 40_000);
    assert_eq!(
        RESULT_OBSERVER_BUDGET_MS - FAKE_TURN_DEADLINE_MS,
        RESULT_OBSERVER_GRACE_MS
    );
}

fn exact_runtime_version(runtime: &Path, python: bool) -> String {
    let output = Command::new(runtime)
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "runtime version query failed for {}",
        runtime.display()
    );
    let raw = if output.stdout.is_empty() {
        &output.stderr
    } else {
        &output.stdout
    };
    let version = String::from_utf8(raw.to_vec()).unwrap();
    let version = version.trim();
    if python {
        version
            .strip_prefix("Python ")
            .expect("python3 --version must have the canonical prefix")
            .to_owned()
    } else {
        assert!(version.starts_with('v'));
        version.to_owned()
    }
}

async fn exercise_external_client(
    binaries: &CandidateBinaries,
    assets: &CrossClientAssets,
    sandbox: &Sandbox,
    language: ClientLanguage,
) {
    let scenario = sandbox.write_client_scenario(language);
    let mut command = Command::new(assets.runtime(language));
    command
        .arg(assets.helper(language))
        .arg(&scenario.config_path)
        .arg(assets.root(language));
    if matches!(language, ClientLanguage::TypeScript) {
        command.arg(&assets.typescript_dist_root);
    }
    command.env_clear();
    if matches!(language, ClientLanguage::Python) {
        command.env("PYTHONDONTWRITEBYTECODE", "1");
    }
    let output = run_bounded(command, Duration::from_secs(180));
    for prompt in scenario.prompts {
        assert_output_does_not_leak(&output, prompt);
    }
    assert_success(&output, &format!("{} public client", language.title()));
    assert!(
        output.stderr.is_empty(),
        "{} client emitted diagnostics on a successful run: {}",
        language.title(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.ends_with(b"\n"));
    let records = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        records.len(),
        1,
        "{} helper must emit exactly one bounded structural report",
        language.title()
    );
    assert!(
        records[0].len() <= 64 * 1024,
        "{} helper report exceeded its 64 KiB evidence bound",
        language.title()
    );
    let report: serde_json::Value = serde_json::from_slice(records[0]).unwrap();
    let report_text = String::from_utf8_lossy(records[0]);
    assert!(!report_text.contains("PMUX_TEST_"));
    assert!(!report_text.contains("\"token\""));
    assert!(!report_text.contains("\"endpoint\""));
    assert_cross_client_report(assets, &scenario, language, &report);

    std::fs::remove_file(&scenario.config_path).unwrap();
    assert!(!scenario.config_path.exists());

    let persistent_launches = sandbox.launches_for_session(scenario.persistent_session);
    assert_eq!(persistent_launches.len(), 2);
    sandbox.assert_plain_launch(
        binaries,
        &persistent_launches[0],
        scenario.persistent_session,
        "new",
        &["--model", "test-model", "--permission-mode", "default"],
    );
    sandbox.assert_plain_launch(
        binaries,
        &persistent_launches[1],
        scenario.persistent_session,
        "resume",
        &["--model", "test-model", "--permission-mode", "default"],
    );
    for launch in &persistent_launches {
        assert_process_boundary_absent(launch, "cross-client persistent Claude process");
    }
    let once_launch = sandbox.only_launch_for_session(scenario.once_session);
    sandbox.assert_plain_launch(
        binaries,
        &once_launch,
        scenario.once_session,
        "new",
        &["--model", "test-model", "--permission-mode", "default"],
    );
    assert_process_boundary_absent(&once_launch, "cross-client run_once Claude process");

    let native = PmuxClient::new(&sandbox.public_socket).unwrap();
    for (session_id, generation) in [
        (
            scenario.persistent_session,
            parse_report_uuid(&report["persistent"]["generation_id"]),
        ),
        (
            scenario.persistent_session,
            parse_report_uuid(&report["resume"]["generation_id"]),
        ),
        (
            scenario.once_session,
            parse_report_uuid(&report["run_once"]["generation_id"]),
        ),
    ] {
        let error = native
            .inspect_session(session_id, generation)
            .await
            .expect_err("client-created session survived an explicit/one-shot close");
        assert_server_code(error, ErrorCode::SessionNotFound);
    }
}

fn assert_cross_client_report(
    assets: &CrossClientAssets,
    scenario: &ClientScenario,
    language: ClientLanguage,
    report: &serde_json::Value,
) {
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["language"], language.label());
    assert_eq!(
        report["runtime"]["path"],
        assets.runtime(language).to_string_lossy().as_ref()
    );
    assert_eq!(
        report["runtime"]["sha256"],
        assets.digest_hex(assets.runtime(language))
    );
    assert_eq!(
        report["runtime"]["version"],
        assets.runtime_version(language)
    );

    let helper = assets.helper(language);
    assert_eq!(report["helper"]["path"], helper.to_string_lossy().as_ref());
    assert_eq!(report["helper"]["sha256"], assets.digest_hex(&helper));
    assert_eq!(report["client"]["package_name"], "pmux-client");
    assert_eq!(report["client"]["package_version"], "0.1.0");
    assert_eq!(report["client"]["protocol_version"], 1);
    if matches!(language, ClientLanguage::TypeScript) {
        assert_eq!(
            report["client"]["source_root"],
            assets.typescript_root.to_string_lossy().as_ref()
        );
        assert_eq!(
            report["client"]["dist_root"],
            assets.typescript_dist_root.to_string_lossy().as_ref()
        );
        assert_eq!(
            report["client"]["dist_sha256"],
            assets.typescript_dist_sha256()
        );
    }
    assert_eq!(
        report["client"]["entry_path"],
        assets.entry(language).to_string_lossy().as_ref()
    );
    let manifest = report["client"]["manifest"].as_array().unwrap();
    assert_eq!(manifest.len(), assets.manifest(language).len());
    for (record, relative) in manifest.iter().zip(assets.manifest(language)) {
        let path = assets.asset(language, relative);
        assert_eq!(record["relative_path"], *relative);
        assert_eq!(record["sha256"], assets.digest_hex(&path));
    }

    assert_eq!(report["ping_protocol_version"], 1);
    assert_eq!(
        report["persistent"]["session_id"],
        scenario.persistent_session.to_string()
    );
    parse_report_uuid(&report["persistent"]["generation_id"]);
    assert_eq!(
        report["persistent"]["first_turn_id"],
        scenario.first_turn.to_string()
    );
    assert!(
        report["persistent"]["first_final_sequence"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert!(
        report["persistent"]["first_event_count"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert_eq!(report["persistent"]["reconnects"], 0);
    assert_eq!(report["persistent"]["inspected"], true);
    assert_eq!(report["persistent"]["closed_and_reaped"], true);
    assert_eq!(
        report["idempotency"]["turn_id"],
        scenario.first_turn.to_string()
    );
    assert_eq!(report["idempotency"]["initial_replayed"], false);
    assert_eq!(report["idempotency"]["replayed"], true);
    assert!(
        report["idempotency"]["replay_final_sequence"]
            .as_u64()
            .is_some_and(|value| value
                > report["persistent"]["first_final_sequence"]
                    .as_u64()
                    .unwrap())
    );
    assert!(
        report["idempotency"]["replay_event_count"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert_eq!(report["idempotency"]["reconnects"], 0);
    assert_eq!(report["idempotency"]["conflict_error_code"], "id_conflict");
    assert_eq!(report["idempotency"]["conflict_preserved_cursor"], true);
    assert_eq!(report["attach"]["metadata_valid"], true);
    assert!(
        report["attach"]["first_stream_bytes"]
            .as_u64()
            .is_some_and(|value| value > 0)
    );
    assert_eq!(report["attach"]["reuse_rejected"], true);

    assert_eq!(
        report["cancellation"]["turn_id"],
        scenario.cancel_turn.to_string()
    );
    assert_eq!(report["cancellation"]["outcome"], "cancelled");
    assert_eq!(report["cancellation"]["recovered_to_ready"], true);
    assert_eq!(
        report["cancellation"]["recovery_turn_id"],
        scenario.recovery_turn.to_string()
    );
    assert_eq!(report["cancellation"]["recovery_outcome"], "completed");

    let persistent_generation = parse_report_uuid(&report["persistent"]["generation_id"]);
    let resume_generation = parse_report_uuid(&report["resume"]["generation_id"]);
    assert_ne!(persistent_generation, resume_generation);
    assert_eq!(
        report["resume"]["stale_error_code"],
        "stale_session_generation"
    );
    assert_eq!(report["resume"]["old_close_replayed"], true);
    assert_eq!(
        report["resume"]["turn_id"],
        scenario.resumed_turn.to_string()
    );
    assert_eq!(report["resume"]["outcome"], "completed");
    assert_eq!(report["resume"]["closed_and_reaped"], true);

    assert_eq!(
        report["run_once"]["session_id"],
        scenario.once_session.to_string()
    );
    parse_report_uuid(&report["run_once"]["generation_id"]);
    assert_eq!(
        report["run_once"]["turn_id"],
        scenario.once_turn.to_string()
    );
    assert_eq!(report["run_once"]["outcome"], "completed");
    assert_eq!(report["run_once"]["text"], "pmux-test-ok");
    assert_eq!(report["missing_socket_transport_error"], true);
}

fn parse_report_uuid(value: &serde_json::Value) -> SessionGenerationId {
    let text = value.as_str().expect("report UUID must be a string");
    let parsed = Uuid::parse_str(text).expect("report UUID must be canonical");
    assert_eq!(parsed.to_string(), text);
    SessionGenerationId::from_uuid(parsed)
}

fn assert_output_does_not_leak(output: &Output, prompt: &str) {
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains(prompt));
        assert!(!text.contains(TEST_ANTHROPIC_SECRET));
        assert!(!text.contains(TEST_PROVIDER_SECRET));
        assert!(!text.contains(TEST_LAUNCH_SECRET));
    }
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed: status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_bounded(mut command: Command, timeout: Duration) -> Output {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    wait_bounded(child, timeout)
}

fn run_bounded_with_stdin(mut command: Command, input: &[u8], timeout: Duration) -> Output {
    let mut child = command.spawn().unwrap();
    child.stdin.as_mut().unwrap().write_all(input).unwrap();
    drop(child.stdin.take());
    wait_bounded(child, timeout)
}

fn wait_bounded(mut child: Child, timeout: Duration) -> Output {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if std::time::Instant::now() >= deadline {
            let pid = child.id();
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "exact child {pid} timed out: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[derive(Debug, PartialEq, Eq)]
struct TermiosSnapshot {
    input_flags: libc::tcflag_t,
    output_flags: libc::tcflag_t,
    control_flags: libc::tcflag_t,
    local_flags: libc::tcflag_t,
    control_characters: Vec<libc::cc_t>,
    input_speed: libc::speed_t,
    output_speed: libc::speed_t,
}

fn open_test_pty(rows: u16, cols: u16) -> (File, File) {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: the output pointers reference initialized writable storage, the
    // optional name/termios pointers are null, and `size` is initialized.
    let result = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    assert_eq!(
        result,
        0,
        "failed to create exact-release attach PTY: {}",
        std::io::Error::last_os_error()
    );
    assert!(master_fd >= 0 && slave_fd >= 0);
    // SAFETY: openpty returned two distinct fresh owned descriptors.
    let master = unsafe { File::from_raw_fd(master_fd) };
    // SAFETY: openpty returned two distinct fresh owned descriptors.
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    (master, slave)
}

fn termios_snapshot(terminal: &File) -> TermiosSnapshot {
    let mut attributes = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `attributes` is writable storage for one termios value and the
    // descriptor is the live slave side of the test-owned PTY.
    let result = unsafe { libc::tcgetattr(terminal.as_raw_fd(), attributes.as_mut_ptr()) };
    assert_eq!(
        result,
        0,
        "failed to inspect exact-release attach PTY: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: successful tcgetattr initialized the entire termios value.
    let attributes = unsafe { attributes.assume_init() };
    // SAFETY: both functions inspect only the initialized termios value.
    let input_speed = unsafe { libc::cfgetispeed(&attributes) };
    // SAFETY: both functions inspect only the initialized termios value.
    let output_speed = unsafe { libc::cfgetospeed(&attributes) };
    TermiosSnapshot {
        input_flags: attributes.c_iflag,
        output_flags: attributes.c_oflag,
        control_flags: attributes.c_cflag,
        local_flags: attributes.c_lflag,
        control_characters: attributes.c_cc.to_vec(),
        input_speed,
        output_speed,
    }
}

fn assert_terminal_restored(restored: &TermiosSnapshot, original: &TermiosSnapshot) {
    assert_eq!(restored.input_flags, original.input_flags);
    assert_eq!(restored.output_flags, original.output_flags);
    assert_eq!(restored.control_flags, original.control_flags);
    // Darwin may set PENDIN while returning to canonical mode even though all
    // stable caller-controlled local flags were restored.
    assert_eq!(
        restored.local_flags & !libc::PENDIN,
        original.local_flags & !libc::PENDIN
    );
    assert_eq!(restored.control_characters, original.control_characters);
    assert_eq!(restored.input_speed, original.input_speed);
    assert_eq!(restored.output_speed, original.output_speed);
}

fn assert_completed(result: &TurnResult, session_id: Uuid, turn_id: Uuid) {
    assert_eq!(result.session_id, session_id);
    assert_eq!(result.turn_id, turn_id);
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(result.text, "pmux-test-ok");
    assert_eq!(result.model.as_deref(), Some("pmux-test-model"));
    assert_eq!(result.usage.main.input_tokens, 3);
    assert_eq!(result.usage.main.output_tokens, 1);
    assert!(result.completion.prompt_acknowledged);
    assert!(result.completion.terminal_message_observed);
    assert!(result.completion.terminal_prompt_observed);
    assert!(result.completion.terminal_quiet_observed);
    assert!(result.completion.transcript_drained);
    assert!(result.compatibility.tested);
}

fn assert_rich_result(result: &TurnResult, session_id: Uuid, turn_id: Uuid) {
    assert_eq!(result.session_id, session_id);
    assert_eq!(result.turn_id, turn_id);
    assert_eq!(result.outcome, TurnOutcome::Completed);
    assert_eq!(result.text, "rich final answer");
    assert_eq!(
        result.final_blocks,
        vec![
            MessageBlock::Text {
                text: "rich final ".to_owned(),
            },
            MessageBlock::Text {
                text: "answer".to_owned(),
            },
        ]
    );
    assert_eq!(result.tools.len(), 1);
    let tool = &result.tools[0];
    assert_eq!(tool.tool_use_id, "pmux-rich-tool-1");
    assert_eq!(tool.name, "Read");
    assert_eq!(
        tool.input,
        serde_json::json!({"file_path": "RICH.md", "line": 7})
    );
    assert_eq!(
        tool.output,
        Some(serde_json::json!({
            "content": "rich tool output",
            "line_count": 1
        }))
    );
    assert_eq!(tool.status, ToolStatus::Completed);
    assert_eq!(tool.started_at_ms, None);
    assert_eq!(tool.completed_at_ms, None);
    assert_eq!(result.model.as_deref(), Some("pmux-rich-final-model"));
    let stop_reason = result.stop_reason.as_ref().expect("rich stop reason");
    assert_eq!(stop_reason.kind, StopReasonKind::EndTurn);
    assert_eq!(stop_reason.raw, None);
    assert_eq!(result.usage.main.input_tokens, 32);
    assert_eq!(result.usage.main.output_tokens, 34);
    assert_eq!(result.usage.main.cache_creation_input_tokens, 36);
    assert_eq!(result.usage.main.cache_read_input_tokens, 38);
    assert_eq!(result.usage.sidechain.input_tokens, 101);
    assert_eq!(result.usage.sidechain.output_tokens, 102);
    assert_eq!(result.usage.sidechain.cache_creation_input_tokens, 103);
    assert_eq!(result.usage.sidechain.cache_read_input_tokens, 104);
    assert_eq!(result.usage.combined.input_tokens, 133);
    assert_eq!(result.usage.combined.output_tokens, 136);
    assert_eq!(result.usage.combined.cache_creation_input_tokens, 139);
    assert_eq!(result.usage.combined.cache_read_input_tokens, 142);
    assert_eq!(result.usage.cost_usd, None);
    assert!(result.timings.submitted_at_ms > 0);
    assert!(
        result
            .timings
            .prompt_acknowledged_at_ms
            .is_some_and(|value| value >= result.timings.submitted_at_ms)
    );
    assert!(
        result
            .timings
            .terminal_candidate_at_ms
            .is_some_and(|value| value >= result.timings.prompt_acknowledged_at_ms.unwrap())
    );
    assert!(result.timings.completed_at_ms >= result.timings.terminal_candidate_at_ms.unwrap());
    assert!(result.timings.drain_ms.is_some_and(|value| value >= 50));
    assert!(result.warnings.is_empty());
    assert_eq!(result.claude_version, PROFILE_VERSION);
    assert!(result.compatibility.tested);
    assert_eq!(result.compatibility.claude_version, PROFILE_VERSION);
    assert_eq!(result.compatibility.os, std::env::consts::OS);
    assert_eq!(result.compatibility.arch, std::env::consts::ARCH);
    assert_eq!(
        result.compatibility.terminal_profile,
        TerminalProfile::Transparent
    );
    assert_eq!(result.compatibility.input_transport, InputTransport::Sdk);
    assert_eq!(result.compatibility.transcript_drain_ms, 50);
    assert_eq!(result.completion.authority, CompletionAuthority::Transcript);
    assert!(result.completion.prompt_acknowledged);
    assert!(result.completion.terminal_message_observed);
    assert!(result.completion.terminal_prompt_observed);
    assert!(result.completion.terminal_quiet_observed);
    assert!(result.completion.transcript_drained);
    assert!(!result.completion.lifecycle_hook_observed);
    // No lifecycle hook, so no Stop instant to publish.
    assert_eq!(result.timings.stop_hook_at_ms, None);
    assert!(result.final_sequence > 0);

    let encoded = serde_json::to_string(result).unwrap();
    for excluded in [
        "pmux-test-ok",
        "rich sidechain must not leak",
        "rich hidden tool thinking",
        "rich hidden final thinking",
        "pmux-rich-tool-model",
        "pmux-sidechain-model",
    ] {
        assert!(
            !encoded.contains(excluded),
            "rich public result leaked non-terminal content: {excluded}"
        );
    }
}

struct SubmittedTurn {
    accepted: TurnAccepted,
    observer_started_at: Instant,
    observer_deadline: Instant,
    semantic_deadline_unix_ms: u64,
}

impl std::ops::Deref for SubmittedTurn {
    type Target = TurnAccepted;

    fn deref(&self) -> &Self::Target {
        &self.accepted
    }
}

async fn submit_turn(client: &PmuxClient, handle: &SessionHandle, prompt: &str) -> SubmittedTurn {
    let observer_started_at = Instant::now();
    let request = turn(Uuid::new_v4(), prompt);
    let semantic_deadline_unix_ms = request
        .deadline_unix_ms
        .expect("full-stack fake turns always carry a semantic deadline");
    let observer_deadline = observer_started_at + Duration::from_millis(RESULT_OBSERVER_BUDGET_MS);
    let mut submission_attempts = 0_u64;
    let mut busy_retries = 0_u64;
    let submission = tokio::time::timeout_at(observer_deadline, async {
        for _ in 0..200 {
            submission_attempts += 1;
            match client
                .run_turn(handle.session_id, handle.generation_id, request.clone())
                .await
            {
                Ok(accepted) => return accepted,
                Err(ClientError::Server(body))
                    if body.code == ErrorCode::SessionBusy && body.retryable =>
                {
                    busy_retries += 1;
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                Err(error) => panic!(
                    "turn submission failed unexpectedly for {}: {error:?}",
                    request.turn_id
                ),
            }
        }
        panic!(
            "turn {} remained busy after {submission_attempts} bounded detach-reconciliation attempts",
            request.turn_id
        )
    })
    .await;

    let accepted = submission.unwrap_or_else(|_| {
        panic!(
            "turn submission timed out at the shared monotonic observer deadline: turn_id={}, semantic_deadline_unix_ms={}, semantic_budget_ms={}, grace_ms={}, observer_budget_ms={}, elapsed_ms={}, submission_attempts={}, busy_retries={}",
            request.turn_id,
            semantic_deadline_unix_ms,
            FAKE_TURN_DEADLINE_MS,
            RESULT_OBSERVER_GRACE_MS,
            RESULT_OBSERVER_BUDGET_MS,
            u64::try_from(observer_started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            submission_attempts,
            busy_retries
        )
    });

    SubmittedTurn {
        accepted,
        observer_started_at,
        observer_deadline,
        semantic_deadline_unix_ms,
    }
}

fn turn(turn_id: Uuid, prompt: &str) -> TurnRequest {
    TurnRequest {
        turn_id,
        prompt: prompt.to_owned(),
        deadline_unix_ms: Some(now_ms() + FAKE_TURN_DEADLINE_MS),
        lease: TurnLeasePolicy {
            on_disconnect: DisconnectAction::Continue,
            heartbeat_timeout_ms: None,
        },
    }
}

async fn wait_for_result(
    client: &PmuxClient,
    handle: &SessionHandle,
    submitted: &SubmittedTurn,
    scenario: &str,
) -> TurnResult {
    let first_sequence = submitted.next_sequence.saturating_sub(1);
    let mut after = first_sequence;
    let mut observed_batches = 0_u64;
    let mut observed_events = 0_u64;

    let observed = tokio::time::timeout_at(submitted.observer_deadline, async {
        loop {
            let remaining = submitted
                .observer_deadline
                .saturating_duration_since(Instant::now());
            let wait_ms = u64::try_from(remaining.as_millis())
                .unwrap_or(u64::MAX)
                .min(RESULT_SUBSCRIBE_WAIT_MS);
            let batch = client
                .subscribe_events(SubscribeEventsRequest {
                    session_id: handle.session_id,
                    generation_id: handle.generation_id,
                    after_sequence: after,
                    wait_ms,
                    max_events: 128,
                })
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "turn {scenario} ({}) result subscription failed after sequence {after}: {error:?}",
                        submitted.turn_id
                    )
                });
            observed_batches += 1;
            assert!(batch.replay_gap.is_none());
            observed_events += u64::try_from(batch.events.len()).unwrap();
            for event in batch.events {
                after = event.sequence;
                match event.event {
                    EventPayload::TurnCompleted(result)
                        if result.turn_id == submitted.turn_id =>
                    {
                        return *result;
                    }
                    EventPayload::TurnFailed(error)
                        if event.turn_id == Some(submitted.turn_id) =>
                    {
                        panic!("turn {scenario} failed unexpectedly: {error:?}");
                    }
                    _ => {}
                }
            }

            // A stream of immediately available event batches must still yield
            // so the absolute timeout can be observed by the executor.
            tokio::task::yield_now().await;
        }
    })
    .await;

    match observed {
        Ok(result) => result,
        Err(_) => panic!(
            "turn {scenario} ({}) did not publish a terminal result before its monotonic observer deadline: semantic_deadline_unix_ms={}, semantic_budget_ms={}, grace_ms={}, observer_budget_ms={}, elapsed_ms={}, first_sequence={}, last_sequence={}, observed_batches={}, observed_events={}",
            submitted.turn_id,
            submitted.semantic_deadline_unix_ms,
            FAKE_TURN_DEADLINE_MS,
            RESULT_OBSERVER_GRACE_MS,
            RESULT_OBSERVER_BUDGET_MS,
            u64::try_from(submitted.observer_started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
            first_sequence,
            after,
            observed_batches,
            observed_events
        ),
    }
}

async fn wait_for_prompt_ack(client: &PmuxClient, handle: &SessionHandle, accepted: &TurnAccepted) {
    let mut after = accepted.next_sequence.saturating_sub(1);
    for _ in 0..30 {
        let batch = client
            .subscribe_events(SubscribeEventsRequest {
                session_id: handle.session_id,
                generation_id: handle.generation_id,
                after_sequence: after,
                wait_ms: 1_000,
                max_events: 128,
            })
            .await
            .unwrap();
        for event in batch.events {
            after = event.sequence;
            if event.turn_id == Some(accepted.turn_id)
                && matches!(event.event, EventPayload::PromptAcknowledged(_))
            {
                return;
            }
        }
    }
    panic!("turn {} was not acknowledged", accepted.turn_id);
}

async fn wait_for_failure(
    client: &PmuxClient,
    handle: &SessionHandle,
    accepted: &TurnAccepted,
) -> pseudomux_protocol::v1::ErrorBody {
    let mut after = accepted.next_sequence.saturating_sub(1);
    for _ in 0..30 {
        let batch = client
            .subscribe_events(SubscribeEventsRequest {
                session_id: handle.session_id,
                generation_id: handle.generation_id,
                after_sequence: after,
                wait_ms: 1_000,
                max_events: 128,
            })
            .await
            .unwrap();
        assert!(batch.replay_gap.is_none());
        for event in batch.events {
            after = event.sequence;
            if event.turn_id == Some(accepted.turn_id) {
                match event.event {
                    EventPayload::TurnFailed(error) => return error,
                    EventPayload::TurnCompleted(result) => {
                        panic!("modal admission turn completed unexpectedly: {result:?}")
                    }
                    _ => {}
                }
            }
        }
    }
    panic!(
        "turn {} did not publish its modal failure",
        accepted.turn_id
    );
}

async fn wait_for_attach_reservation_release(
    client: &PmuxClient,
    handle: &SessionHandle,
) -> pseudomux_protocol::v1::AttachCapability {
    for _ in 0..200 {
        match client
            .attach_session(AttachSessionRequest {
                session_id: handle.session_id,
                generation_id: handle.generation_id,
                read_only: false,
                size: None,
            })
            .await
        {
            Ok(capability) => return capability,
            Err(ClientError::Server(error))
                if error.code == ErrorCode::SessionBusy && error.retryable =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => panic!("attach reconciliation failed unexpectedly: {error:?}"),
        }
    }
    panic!("attach reservation remained held after bounded needs-input reconciliation");
}

async fn consume_attach_capability(endpoint: &str, token: &str) {
    let mut stream = UnixStream::connect(endpoint).await.unwrap();
    let token_bytes = token.as_bytes();
    stream
        .write_all(&(token_bytes.len() as u32).to_be_bytes())
        .await
        .unwrap();
    stream.write_all(token_bytes).await.unwrap();
    stream.flush().await.unwrap();
    let mut first_bytes = [0_u8; 16];
    let read = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut first_bytes))
        .await
        .unwrap()
        .unwrap();
    assert!(read > 0, "attach proxy did not return terminal bytes");
    stream.shutdown().await.unwrap();
}

async fn answer_attach_capability(endpoint: &str, token: &str, answer: &[u8]) {
    let mut stream = UnixStream::connect(endpoint).await.unwrap();
    let token_bytes = token.as_bytes();
    stream
        .write_all(&(token_bytes.len() as u32).to_be_bytes())
        .await
        .unwrap();
    stream.write_all(token_bytes).await.unwrap();

    let mut frame = Vec::with_capacity(5 + answer.len());
    frame.push(1);
    frame.extend_from_slice(&u32::try_from(answer.len()).unwrap().to_le_bytes());
    frame.extend_from_slice(answer);
    stream.write_all(&frame).await.unwrap();
    stream.flush().await.unwrap();
    stream.shutdown().await.unwrap();
}

async fn wait_for_turn_needs_input(
    client: &PmuxClient,
    handle: &SessionHandle,
) -> pseudomux_protocol::v1::SessionSnapshot {
    for _ in 0..400 {
        let snapshot = client
            .inspect_session(handle.session_id, handle.generation_id)
            .await
            .unwrap();
        if snapshot.state == pseudomux_protocol::v1::SessionState::NeedsInput {
            return snapshot;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("turn did not enter needs_input through the real PTY boundary");
}

fn assert_server_code(error: ClientError, expected: ErrorCode) {
    match error {
        ClientError::Server(body) => assert_eq!(body.code, expected),
        other => panic!("expected server error {expected:?}, got {other:?}"),
    }
}

fn assert_retryable_server_code(error: ClientError, expected: ErrorCode) {
    match error {
        ClientError::Server(body) => {
            assert_eq!(body.code, expected);
            assert!(body.retryable, "{expected:?} must remain retryable");
        }
        other => panic!("expected retryable server error {expected:?}, got {other:?}"),
    }
}

async fn wait_for_file(path: &Path, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if path.is_file() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for exact fixture file {}",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[derive(Clone, Debug)]
struct ExactProcessIdentity {
    pid: u32,
    start_identity: String,
    executable_path: PathBuf,
    process_group_id: i32,
    session_id: i32,
}

impl ExactProcessIdentity {
    fn capture(pid: u32, expected_executable: &Path) -> Self {
        assert!(pid > 0);
        let start_identity = process_start_identity(pid)
            .unwrap()
            .expect("exact process disappeared during identity capture");
        let executable_path = process_executable_path(pid).unwrap();
        assert_eq!(executable_path, expected_executable);
        let process_group_id = exact_process_group_id(pid)
            .unwrap()
            .expect("exact process group disappeared during identity capture");
        let session_id = exact_process_session_id(pid)
            .unwrap()
            .expect("exact process session disappeared during identity capture");
        assert_eq!(
            process_start_identity(pid).unwrap().as_deref(),
            Some(start_identity.as_str()),
            "process identity changed during capture"
        );
        Self {
            pid,
            start_identity,
            executable_path,
            process_group_id,
            session_id,
        }
    }

    fn assert_running(&self) {
        assert_eq!(
            process_start_identity(self.pid).unwrap().as_deref(),
            Some(self.start_identity.as_str()),
            "exact process {} is absent or its PID was reused",
            self.pid
        );
        assert_eq!(
            process_executable_path(self.pid).unwrap(),
            self.executable_path
        );
        assert_eq!(
            exact_process_group_id(self.pid).unwrap(),
            Some(self.process_group_id)
        );
        assert_eq!(
            exact_process_session_id(self.pid).unwrap(),
            Some(self.session_id)
        );
    }

    fn assert_same_process(&self, other: &Self) {
        assert_eq!(self.pid, other.pid);
        assert_eq!(self.start_identity, other.start_identity);
        assert_eq!(self.executable_path, other.executable_path);
    }

    fn signal(&self, signal: libc::c_int) {
        self.assert_running();
        let pid = libc::pid_t::try_from(self.pid).unwrap();
        // SAFETY: PID reuse, executable, process-group, and POSIX-session
        // identity were revalidated immediately above for this retained child.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
    }
}

fn exact_process_identity_from_launch(
    launch: &serde_json::Value,
    executable: &Path,
) -> ExactProcessIdentity {
    let pid = launch["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .expect("launch records a bounded child PID");
    let identity = ExactProcessIdentity::capture(pid, executable);
    assert_eq!(
        launch["process_start_identity"].as_str(),
        Some(identity.start_identity.as_str())
    );
    assert_eq!(
        launch["process_group_id"].as_i64(),
        Some(i64::from(identity.process_group_id))
    );
    assert_eq!(
        launch["process_session_id"].as_i64(),
        Some(i64::from(identity.session_id))
    );
    identity
}

struct ExactProcessCleanupGuard {
    identity: Option<ExactProcessIdentity>,
}

impl ExactProcessCleanupGuard {
    fn new(identity: ExactProcessIdentity) -> Self {
        Self {
            identity: Some(identity),
        }
    }

    fn identity(&self) -> &ExactProcessIdentity {
        self.identity.as_ref().expect("cleanup guard remains armed")
    }

    fn update(&mut self, identity: ExactProcessIdentity) {
        self.identity().assert_same_process(&identity);
        self.identity = Some(identity);
    }

    fn disarm(&mut self) {
        self.identity = None;
    }
}

impl Drop for ExactProcessCleanupGuard {
    fn drop(&mut self) {
        let Some(identity) = self.identity.as_ref() else {
            return;
        };
        if process_start_identity(identity.pid)
            .ok()
            .flatten()
            .as_deref()
            != Some(identity.start_identity.as_str())
            || process_executable_path(identity.pid).ok().as_deref()
                != Some(identity.executable_path.as_path())
        {
            return;
        }
        let pid = match libc::pid_t::try_from(identity.pid) {
            Ok(pid) => pid,
            Err(_) => return,
        };
        if exact_process_group_id(identity.pid).ok().flatten() == Some(pid)
            && exact_process_session_id(identity.pid).ok().flatten() == Some(pid)
        {
            // SAFETY: the exact still-live session leader fences this negative
            // process-group signal to the test-owned session.
            let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        // SAFETY: start identity and executable path were revalidated above.
        let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

async fn wait_for_exact_process_absence(identity: &ExactProcessIdentity, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match process_start_identity(identity.pid).unwrap() {
            None => return,
            Some(actual) if actual != identity.start_identity => return,
            Some(_) => {}
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "exact process {} survived cleanup",
            identity.pid
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

struct CandidateBinaries {
    directory: PathBuf,
    directory_device: u64,
    directory_inode: u64,
    pmuxd: PathBuf,
    rmuxd: PathBuf,
    launcher: PathBuf,
    pmux: PathBuf,
    mcp: PathBuf,
    claude_p: PathBuf,
    hook: PathBuf,
    fake_claude: PathBuf,
    identities: BTreeMap<PathBuf, ExecutableIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExecutableIdentity {
    sha256: [u8; 32],
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    links: u64,
}

impl CandidateBinaries {
    fn from_environment() -> Self {
        let directory = PathBuf::from(
            std::env::var_os("PMUX_E2E_BIN_DIR")
                .expect("PMUX_E2E_BIN_DIR must identify the exact candidate directory"),
        )
        .canonicalize()
        .unwrap();
        assert!(directory.is_absolute());
        let directory_metadata = std::fs::metadata(&directory).unwrap();
        assert!(directory_metadata.is_dir());
        let pmuxd = required_executable(&directory, "pmuxd");
        let rmuxd = required_executable(&directory, "pmux-rmuxd");
        let launcher = required_executable(&directory, "pmux-launcher");
        let pmux = required_executable(&directory, "pmux");
        let mcp = required_executable(&directory, "pmux-mcp");
        let claude_p = required_executable(&directory, "claude-p");
        let hook = required_executable(&directory, "pmux-hook");
        let fake_claude = required_executable(&directory, "pmux-test-claude");
        let identities = [
            &pmuxd,
            &rmuxd,
            &launcher,
            &pmux,
            &mcp,
            &claude_p,
            &hook,
            &fake_claude,
        ]
        .into_iter()
        .map(|path| (path.clone(), executable_identity(path)))
        .collect::<BTreeMap<_, _>>();
        let unique_files = identities
            .values()
            .map(|identity| (identity.device, identity.inode))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            unique_files.len(),
            identities.len(),
            "candidate names must identify distinct executable files"
        );
        Self {
            pmuxd,
            rmuxd,
            launcher,
            pmux,
            mcp,
            claude_p,
            hook,
            fake_claude,
            directory_device: directory_metadata.dev(),
            directory_inode: directory_metadata.ino(),
            directory,
            identities,
        }
    }

    fn assert_unchanged(&self) {
        let directory_metadata = std::fs::metadata(&self.directory).unwrap();
        assert!(directory_metadata.is_dir());
        assert_eq!(directory_metadata.dev(), self.directory_device);
        assert_eq!(directory_metadata.ino(), self.directory_inode);
        for (path, expected) in &self.identities {
            assert_eq!(
                &executable_identity(path),
                expected,
                "candidate executable changed during E2E: {}",
                path.display()
            );
        }
        assert_eq!(self.hook.parent(), Some(self.directory.as_path()));
    }

    fn digest_hex(&self, path: &Path) -> String {
        let digest = self
            .identities
            .get(path)
            .expect("candidate path has a frozen executable identity")
            .sha256;
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

fn digest_file(path: &Path) -> [u8; 32] {
    let bytes = std::fs::read(path).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

fn executable_identity(path: &Path) -> ExecutableIdentity {
    let metadata = std::fs::metadata(path).unwrap();
    ExecutableIdentity {
        sha256: digest_file(path),
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        mode: metadata.mode(),
        links: metadata.nlink(),
    }
}

fn required_executable(directory: &Path, name: &str) -> PathBuf {
    let path = directory.join(name).canonicalize().unwrap();
    assert_eq!(path.parent(), Some(directory));
    let metadata = std::fs::metadata(&path).unwrap();
    assert!(metadata.is_file());
    assert_ne!(metadata.permissions().mode() & 0o111, 0);
    assert_eq!(metadata.nlink(), 1, "candidate executable is hard-linked");
    path
}

struct Sandbox {
    root: TempDir,
    root_path: PathBuf,
    public_socket: PathBuf,
    runtime_parent: PathBuf,
    cwd: PathBuf,
    config_root: PathBuf,
    state_root: PathBuf,
    path_safe_first: PathBuf,
    path_safe_last: PathBuf,
    shim_dir: PathBuf,
    plugin_dir: PathBuf,
    fake_claude: PathBuf,
}

impl Sandbox {
    fn new(binaries: &CandidateBinaries) -> Self {
        let root = tempfile::Builder::new()
            .prefix("pmux-e2e-")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let runtime_parent = root_path.join("private");
        let cwd = root_path.join("workspace");
        let config_root = root_path.join("config");
        let state_root = root_path.join("state");
        let path_safe_first = root_path.join("path-first");
        let path_safe_last = root_path.join("path-last");
        let shim_dir = root_path.join("parent-tmux-shim");
        let plugin_dir = root_path.join("plugin");
        for directory in [
            &runtime_parent,
            &cwd,
            &config_root,
            &state_root,
            &path_safe_first,
            &path_safe_last,
            &shim_dir,
            &plugin_dir,
        ] {
            std::fs::create_dir(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self {
            public_socket: root_path.join("pmux.sock"),
            fake_claude: binaries.fake_claude.clone(),
            root,
            root_path,
            runtime_parent,
            cwd,
            config_root,
            state_root,
            path_safe_first,
            path_safe_last,
            shim_dir,
            plugin_dir,
        }
    }

    /// A fresh, empty, owner-only directory for one minified cell's private
    /// Claude configuration root.
    ///
    /// Per CELL and never shared: `history.jsonl`, `paste-cache/` and
    /// `projects/` are per-root, so two cells on one root would accumulate each
    /// other's prompts however clean their transcripts were. The daemon refuses
    /// a minified start whose root is not both unshared and unused, so this
    /// creating a new directory per call is the contract and not tidiness.
    fn private_config_root(&self, label: &str) -> PathBuf {
        let root = self.root_path.join(format!("private-config-{label}"));
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    fn start_request(
        &self,
        identity: SessionIdentity,
        retention: RetentionPolicy,
    ) -> StartSessionRequest {
        StartSessionRequest {
            identity,
            cwd: self.cwd.to_string_lossy().into_owned(),
            agent: None,
            claude: Some(ClaudeLaunchConfig {
                executable: self.fake_claude.to_string_lossy().into_owned(),
                model: Some("test-model".into()),
                effort: None,
                permission_mode: Some(PermissionMode::Default),
                allowed_tools: Vec::new(),
                denied_tools: Vec::new(),
                settings: Vec::new(),
                mcp_configs: Vec::new(),
                plugin_dirs: Vec::new(),
                system_prompt: SystemPromptPolicy::Default,
                extra_args: Vec::new(),
            }),
            environment: self.environment_spec(),
            auth_policy: AuthPolicy::Subscription,
            config_isolation: None,
            terminal: TerminalSpec {
                rows: 24,
                cols: 120,
                profile: TerminalProfile::Transparent,
                input_transport: InputTransport::Sdk,
            },
            lifecycle: LifecycleMode::Transcript,
            retention,
            compatibility: CompatibilityPolicy::RequireTested,
            cell: SessionCell::Full,
        }
    }

    fn attested_start_request(
        &self,
        identity: SessionIdentity,
        retention: RetentionPolicy,
    ) -> StartSessionRequest {
        let mut request = self.start_request(identity, retention);
        request.claude.as_mut().expect("inline launch").effort = Some(EffortLevel::Medium);
        request
            .claude
            .as_mut()
            .expect("inline launch")
            .permission_mode = Some(PermissionMode::Plan);
        request
            .claude
            .as_mut()
            .expect("inline launch")
            .allowed_tools = vec!["Read".into(), "Bash".into()];
        request.claude.as_mut().expect("inline launch").denied_tools = vec!["WebFetch".into()];
        request.claude.as_mut().expect("inline launch").settings = vec![ConfigSource::Inline {
            document: serde_json::json!({"synthetic_secret": TEST_LAUNCH_SECRET}),
        }];
        request.claude.as_mut().expect("inline launch").mcp_configs = vec![ConfigSource::Inline {
            document: serde_json::json!({"synthetic_token": TEST_LAUNCH_SECRET}),
        }];
        request.claude.as_mut().expect("inline launch").plugin_dirs =
            vec![self.plugin_dir.to_string_lossy().into_owned()];
        request
            .claude
            .as_mut()
            .expect("inline launch")
            .system_prompt = SystemPromptPolicy::Replace {
            prompt: TEST_LAUNCH_SECRET.into(),
        };
        request.claude.as_mut().expect("inline launch").extra_args =
            vec!["--debug".into(), "--verbose".into()];
        request
    }

    fn environment_spec(&self) -> EnvironmentSpec {
        let mut snapshot = self.raw_environment_snapshot();
        snapshot.insert("PMUX_TEST_PATCH_ORDER".into(), "snapshot-value".into());
        snapshot.insert("PMUX_TEST_UNSET_ME".into(), "must-be-removed".into());
        for key in TEST_SUBSCRIPTION_KEYS {
            let value = if key.starts_with("ANTHROPIC") {
                TEST_ANTHROPIC_SECRET
            } else {
                TEST_PROVIDER_SECRET
            };
            snapshot.insert((*key).into(), value.into());
        }
        for key in TEST_TRANSPARENT_EXACT_KEYS {
            let value = if *key == "TMUX_PROGRAM" {
                self.shim_dir.join("tmux").to_string_lossy().into_owned()
            } else {
                format!("ambient-{key}")
            };
            snapshot.insert((*key).into(), value);
        }
        snapshot.extend([
            ("RMUX_TEST_BOUNDARY".into(), "must-strip".into()),
            ("TMUX_TEST_BOUNDARY".into(), "must-strip".into()),
            ("CLAUDE_AGENT_SDK_TEST_BOUNDARY".into(), "must-strip".into()),
            ("CLAUDE_CODE_SDK_TEST_BOUNDARY".into(), "must-strip".into()),
        ]);

        EnvironmentSpec {
            snapshot,
            set: BTreeMap::from([
                (
                    "PMUX_TEST_PATCH_ORDER".into(),
                    TEST_ENV_PATCHED_VALUE.into(),
                ),
                ("PMUX_TEST_SET_ONLY".into(), TEST_ENV_SET_ONLY_VALUE.into()),
                ("ANTHROPIC_API_KEY".into(), TEST_ANTHROPIC_SECRET.into()),
            ]),
            unset: BTreeSet::from([
                "PMUX_TEST_PATCH_ORDER".into(),
                "PMUX_TEST_UNSET_ME".into(),
                "ANTHROPIC_API_KEY".into(),
            ]),
        }
    }

    fn raw_environment_snapshot(&self) -> BTreeMap<String, String> {
        let path_with_shim = std::env::join_paths([
            self.path_safe_first.as_path(),
            self.shim_dir.as_path(),
            self.path_safe_last.as_path(),
        ])
        .unwrap()
        .into_string()
        .unwrap();
        let expected_path = std::env::join_paths([
            self.path_safe_first.as_path(),
            self.path_safe_last.as_path(),
        ])
        .unwrap()
        .into_string()
        .unwrap();
        BTreeMap::from([
            ("HOME".into(), self.root_path.to_string_lossy().into_owned()),
            (
                "CLAUDE_CONFIG_DIR".into(),
                self.config_root.to_string_lossy().into_owned(),
            ),
            (
                "PMUX_TEST_STATE_DIR".into(),
                self.state_root.to_string_lossy().into_owned(),
            ),
            (
                "PMUX_TEST_ENV_ATTESTATION".into(),
                TEST_ENV_ATTESTATION_MARKER.into(),
            ),
            (
                "PMUX_TEST_CALLER_SAFE_CONFIG".into(),
                TEST_ENV_SAFE_CONFIG_VALUE.into(),
            ),
            ("PMUX_TEST_EXPECTED_PATH".into(), expected_path),
            ("PATH".into(), path_with_shim),
            ("TERM".into(), "ambient-terminal".into()),
        ])
    }

    fn configure_external_environment(&self, command: &mut Command) {
        let mut environment = self.environment_spec().snapshot;
        environment.remove("PMUX_TEST_UNSET_ME");
        environment.insert(
            "PMUX_TEST_PATCH_ORDER".into(),
            TEST_ENV_PATCHED_VALUE.into(),
        );
        environment.insert("PMUX_TEST_SET_ONLY".into(), TEST_ENV_SET_ONLY_VALUE.into());
        environment.insert("ANTHROPIC_API_KEY".into(), TEST_ANTHROPIC_SECRET.into());
        command.env_clear().envs(environment);
    }

    fn write_client_scenario(&self, language: ClientLanguage) -> ClientScenario {
        let persistent_session = Uuid::new_v4();
        let first_turn = Uuid::new_v4();
        let cancel_turn = Uuid::new_v4();
        let recovery_turn = Uuid::new_v4();
        let resumed_turn = Uuid::new_v4();
        let once_session = Uuid::new_v4();
        let once_turn = Uuid::new_v4();
        let prompts = language.prompts();
        let config = serde_json::json!({
            "schema_version": 1,
            "socket_path": self.public_socket,
            "claude_executable": self.fake_claude,
            "cwd": self.cwd,
            "environment": self.environment_spec(),
            "ids": {
                "persistent_session": persistent_session,
                "first_turn": first_turn,
                "cancel_turn": cancel_turn,
                "recovery_turn": recovery_turn,
                "resumed_turn": resumed_turn,
                "once_session": once_session,
                "once_turn": once_turn,
            },
            "prompts": {
                "first": prompts[0],
                "cancel": prompts[1],
                "recovery": prompts[2],
                "resumed": prompts[3],
                "once": prompts[4],
            }
        });
        let config_path = self
            .root_path
            .join(format!("{}-client-input.json", language.label()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&config_path)
            .unwrap();
        file.write_all(&serde_json::to_vec(&config).unwrap())
            .unwrap();
        file.sync_all().unwrap();
        assert_eq!(
            std::fs::metadata(&config_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        ClientScenario {
            config_path,
            persistent_session,
            first_turn,
            cancel_turn,
            recovery_turn,
            resumed_turn,
            once_session,
            once_turn,
            prompts,
        }
    }

    fn launches(&self) -> Vec<serde_json::Value> {
        let launches = std::fs::read_to_string(self.state_root.join("launches.jsonl")).unwrap();
        launches
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect()
    }

    fn launch_count(&self) -> usize {
        std::fs::read_to_string(self.state_root.join("launches.jsonl"))
            .map(|launches| launches.lines().count())
            .unwrap_or(0)
    }

    fn launches_for_session(&self, session_id: Uuid) -> Vec<serde_json::Value> {
        self.launches()
            .into_iter()
            .filter(|launch| launch["session_id"] == session_id.to_string())
            .collect()
    }

    fn only_launch_for_session(&self, session_id: Uuid) -> serde_json::Value {
        let matches = self.launches_for_session(session_id);
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one launch for session {session_id}"
        );
        matches.into_iter().next().unwrap()
    }

    fn assert_common_launch(
        &self,
        binaries: &CandidateBinaries,
        launch: &serde_json::Value,
        session_id: Uuid,
        mode: &str,
    ) {
        assert_eq!(launch["attestation_version"], TEST_ATTESTATION_VERSION);
        assert_eq!(launch["session_id"], session_id.to_string());
        assert_eq!(launch["mode"], mode);
        assert_eq!(launch["cwd"], self.cwd.to_string_lossy().as_ref());
        assert_eq!(launch["forbidden_flag_count"], 0);
        assert_eq!(
            launch["executable_path"],
            binaries.fake_claude.to_string_lossy().as_ref()
        );
        assert_eq!(
            launch["executable_sha256"],
            binaries.digest_hex(&binaries.fake_claude)
        );
        let pid = launch["pid"].as_i64().expect("child pid is recorded");
        assert!(pid > 0);
        assert_eq!(launch["process_group_id"], pid);
        assert_eq!(launch["process_session_id"], pid);
        assert!(launch["process_start_identity"].is_string());

        let argv = launch["argv"].as_array().expect("exact argv is recorded");
        assert_eq!(
            launch["argv_count"].as_u64(),
            Some(u64::try_from(argv.len()).unwrap())
        );
        for value in argv.iter().filter_map(serde_json::Value::as_str) {
            assert!(!value.contains(TEST_ANTHROPIC_SECRET));
            assert!(!value.contains(TEST_PROVIDER_SECRET));
            assert!(!value.contains(TEST_LAUNCH_SECRET));
            assert!(
                !value.contains("PMUX_TEST_"),
                "prompt-like sentinel leaked into Claude argv: {value}"
            );
        }

        let environment = &launch["environment"];
        assert_eq!(
            environment["attestation_marker"],
            TEST_ENV_ATTESTATION_MARKER
        );
        assert_eq!(environment["patch_order"], TEST_ENV_PATCHED_VALUE);
        assert_eq!(environment["set_only"], TEST_ENV_SET_ONLY_VALUE);
        assert_eq!(
            environment["caller_safe_config"],
            TEST_ENV_SAFE_CONFIG_VALUE
        );
        assert_eq!(environment["unset_present"], false);
        assert_eq!(environment["forbidden_keys_present"], serde_json::json!([]));
        assert_eq!(environment["stripped_secret_values_present"], false);
        assert_eq!(environment["term"], "xterm-256color");
        assert_eq!(
            environment["path"],
            self.expected_transparent_path().as_str()
        );
        assert_eq!(
            environment["home"],
            self.root_path.to_string_lossy().as_ref()
        );
        assert_eq!(
            environment["claude_config_dir"],
            self.config_root.to_string_lossy().as_ref()
        );
        assert_eq!(
            environment["state_dir"],
            self.state_root.to_string_lossy().as_ref()
        );
    }

    fn assert_plain_launch(
        &self,
        binaries: &CandidateBinaries,
        launch: &serde_json::Value,
        session_id: Uuid,
        mode: &str,
        expected_tail: &[&str],
    ) {
        self.assert_common_launch(binaries, launch, session_id, mode);
        let identity_flag = if mode == "resume" {
            "--resume"
        } else {
            "--session-id"
        };
        let mut expected = vec![identity_flag.to_owned(), session_id.to_string()];
        expected.extend(expected_tail.iter().map(|value| (*value).to_owned()));
        let actual = launch["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    fn assert_hybrid_launch(
        &self,
        binaries: &CandidateBinaries,
        launch: &serde_json::Value,
        session_id: Uuid,
    ) -> PathBuf {
        self.assert_common_launch(binaries, launch, session_id, "new");
        let argv = launch["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(argv.len(), 8);
        assert_eq!(argv[0], "--session-id");
        assert_eq!(argv[1], session_id.to_string());
        assert_eq!(
            &argv[2..7],
            [
                "--model",
                "test-model",
                "--permission-mode",
                "default",
                "--settings",
            ]
        );
        let settings = PathBuf::from(argv[7]);
        assert!(settings.starts_with(&self.runtime_parent));
        assert_eq!(
            std::fs::metadata(&settings).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
        for event in ["SessionStart", "Stop", "StopFailure"] {
            let entries = document["hooks"][event]
                .as_array()
                .expect("Hybrid lifecycle event must be an array");
            assert_eq!(entries.len(), 1);
            let command = entries[0]["hooks"][0]["command"].as_str().unwrap();
            assert!(command.contains(binaries.hook.to_string_lossy().as_ref()));
            assert!(command.contains(&format!("--event '{event}'")));
        }
        settings
    }

    fn assert_rich_launch(
        &self,
        binaries: &CandidateBinaries,
        launch: &serde_json::Value,
        session_id: Uuid,
        mode: &str,
    ) -> Vec<PathBuf> {
        self.assert_common_launch(binaries, launch, session_id, mode);
        let argv = launch["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(argv.len(), 24);
        assert_eq!(argv[0], "--session-id");
        assert_eq!(argv[1], session_id.to_string());
        assert_eq!(
            &argv[2..15],
            [
                "--model",
                "test-model",
                "--effort",
                "medium",
                "--permission-mode",
                "plan",
                "--allowedTools",
                "Read",
                "--allowedTools",
                "Bash",
                "--disallowedTools",
                "WebFetch",
                "--settings",
            ]
        );
        assert_eq!(argv[16], "--mcp-config");
        assert_eq!(argv[18], "--plugin-dir");
        assert_eq!(
            Path::new(argv[19]).canonicalize().unwrap(),
            self.plugin_dir.canonicalize().unwrap()
        );
        assert_eq!(&argv[20..22], ["--debug", "--verbose"]);
        assert_eq!(argv[22], "--system-prompt-file");

        let settings = PathBuf::from(argv[15]);
        let mcp = PathBuf::from(argv[17]);
        let system_prompt = PathBuf::from(argv[23]);
        for path in [&settings, &mcp, &system_prompt] {
            assert!(path.starts_with(&self.runtime_parent));
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&settings).unwrap())
                .unwrap(),
            serde_json::json!({"synthetic_secret": TEST_LAUNCH_SECRET})
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&mcp).unwrap()).unwrap(),
            serde_json::json!({"synthetic_token": TEST_LAUNCH_SECRET})
        );
        assert_eq!(
            std::fs::read_to_string(&system_prompt).unwrap(),
            TEST_LAUNCH_SECRET
        );
        vec![settings, mcp, system_prompt]
    }

    fn expected_transparent_path(&self) -> String {
        std::env::join_paths([
            self.path_safe_first.as_path(),
            self.path_safe_last.as_path(),
        ])
        .unwrap()
        .into_string()
        .unwrap()
    }

    fn assert_clean(&self, binaries: &CandidateBinaries, sensitive_attach_token: &str) {
        assert!(
            !self.public_socket.exists(),
            "public socket survived daemon shutdown"
        );
        assert_eq!(std::fs::read_dir(&self.runtime_parent).unwrap().count(), 0);
        let log = std::fs::read_to_string(self.root_path.join("logs/pmuxd.log")).unwrap();
        assert!(!log.contains("PMUX_TEST_"));
        assert!(!log.contains(sensitive_attach_token));
        for sensitive in [
            TEST_ANTHROPIC_SECRET,
            TEST_PROVIDER_SECRET,
            TEST_LAUNCH_SECRET,
        ] {
            assert!(!log.contains(sensitive));
        }
        let stderr = std::fs::read_to_string(self.root_path.join("pmuxd.stderr")).unwrap();
        assert!(!stderr.contains("PMUX_TEST_"));
        assert!(!stderr.contains(sensitive_attach_token));
        for sensitive in [
            TEST_ANTHROPIC_SECRET,
            TEST_PROVIDER_SECRET,
            TEST_LAUNCH_SECRET,
        ] {
            assert!(!stderr.contains(sensitive));
        }

        let launches = std::fs::read_to_string(self.state_root.join("launches.jsonl")).unwrap();
        assert_eq!(
            launches.lines().count(),
            EXPECTED_CLAUDE_LAUNCHES,
            "an unexpected or missing Claude process launch escaped per-session accounting"
        );
        let mut process_identities = BTreeSet::new();
        for line in launches.lines() {
            let launch: serde_json::Value = serde_json::from_str(line).unwrap();
            let session_id = Uuid::parse_str(launch["session_id"].as_str().unwrap()).unwrap();
            let mode = launch["mode"].as_str().unwrap();
            self.assert_common_launch(binaries, &launch, session_id, mode);
            assert!(
                process_identities.insert((
                    launch["pid"].as_u64().unwrap(),
                    launch["process_start_identity"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                )),
                "two launches reported the same exact process identity"
            );
            assert_process_boundary_absent(&launch, "recorded Claude process");
        }
        assert!(!launches.contains("PMUX_TEST_"));
        assert!(!launches.contains(TEST_ANTHROPIC_SECRET));
        assert!(!launches.contains(TEST_PROVIDER_SECRET));
        assert!(!launches.contains(TEST_LAUNCH_SECRET));
        let _keep_root_alive = &self.root;
    }
}

fn assert_process_boundary_absent(launch: &serde_json::Value, label: &str) {
    let pid = launch["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .expect("recorded child pid fits u32");
    let expected = launch["process_start_identity"]
        .as_str()
        .expect("launch records an exact process start identity");
    match process_start_identity(pid).expect("query exact process start identity") {
        None => {}
        Some(actual) if actual != expected => {
            // The numeric PID was reused; the recorded process is nevertheless
            // absent and this test must not target the unrelated replacement.
        }
        Some(actual) => {
            panic!(
                "{label} pid {pid} with exact start identity {actual} still exists after committed cleanup"
            );
        }
    }

    let process_group_id = launch["process_group_id"]
        .as_i64()
        .expect("launch records its process group");
    let process_session_id = i32::try_from(
        launch["process_session_id"]
            .as_i64()
            .expect("launch records its process session"),
    )
    .expect("recorded process session fits pid_t");
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,pgid=,state="])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "failed to inspect process boundaries"
    );
    for row in String::from_utf8(output.stdout).unwrap().lines() {
        let fields = row.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 3 {
            continue;
        }
        let observed_pid = fields[0].parse::<i64>().unwrap();
        let observed_group = fields[1].parse::<i64>().unwrap();
        let observed_session = u32::try_from(observed_pid)
            .ok()
            .and_then(|pid| exact_process_session_id(pid).unwrap());
        assert!(
            observed_group != process_group_id && observed_session != Some(process_session_id),
            "{label} left process boundary member pid={observed_pid} pgid={observed_group} sid={observed_session:?} state={}",
            fields[2]
        );
    }
}

#[cfg(target_os = "linux")]
fn process_start_identity(pid: u32) -> std::io::Result<Option<String>> {
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let (_, fields) = stat.rsplit_once(") ").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "malformed /proc process stat",
        )
    })?;
    let start_ticks = fields.split_whitespace().nth(19).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "missing process start time in /proc stat",
        )
    })?;
    Ok(Some(format!("linux_boot_ticks:{start_ticks}")))
}

#[cfg(target_os = "linux")]
fn process_executable_path(pid: u32) -> std::io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe"))?.canonicalize()
}

#[cfg(target_os = "macos")]
fn process_start_identity(pid: u32) -> std::io::Result<Option<String>> {
    let pid =
        libc::c_int::try_from(pid).map_err(|_| std::io::Error::other("pid does not fit c_int"))?;
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>())
        .map_err(|_| std::io::Error::other("proc_bsdinfo size does not fit c_int"))?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: `info` is writable storage of the exact requested flavor size.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read < size {
        return Ok(None);
    }
    // SAFETY: proc_pidinfo reported a complete proc_bsdinfo structure.
    let info = unsafe { info.assume_init() };
    Ok(Some(format!(
        "macos_timeval:{}:{}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    )))
}

#[cfg(target_os = "macos")]
fn process_executable_path(pid: u32) -> std::io::Result<PathBuf> {
    let pid =
        libc::c_int::try_from(pid).map_err(|_| std::io::Error::other("pid does not fit c_int"))?;
    let mut path = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `path` is writable for its reported capacity and proc_pidpath
    // writes at most that many bytes for the exact retained child PID.
    let written = unsafe {
        libc::proc_pidpath(
            pid,
            path.as_mut_ptr().cast(),
            u32::try_from(path.len()).unwrap(),
        )
    };
    if written <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    path.truncate(usize::try_from(written).unwrap());
    if path.last() == Some(&0) {
        path.pop();
    }
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(path)).canonicalize()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_identity(_pid: u32) -> std::io::Result<Option<String>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "exact process start identity is unsupported on this platform",
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_executable_path(_pid: u32) -> std::io::Result<PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "exact process executable identity is unsupported on this platform",
    ))
}

fn exact_process_group_id(pid: u32) -> std::io::Result<Option<i32>> {
    let pid =
        libc::pid_t::try_from(pid).map_err(|_| std::io::Error::other("pid does not fit pid_t"))?;
    // SAFETY: getpgid reads kernel metadata for one positive retained PID.
    let process_group_id = unsafe { libc::getpgid(pid) };
    if process_group_id >= 0 {
        return Ok(Some(process_group_id));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(error)
    }
}

fn exact_process_session_id(pid: u32) -> std::io::Result<Option<i32>> {
    let pid =
        libc::pid_t::try_from(pid).map_err(|_| std::io::Error::other("pid does not fit pid_t"))?;
    // SAFETY: getsid reads kernel metadata for one positive retained PID.
    let session_id = unsafe { libc::getsid(pid) };
    if session_id >= 0 {
        return Ok(Some(session_id));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(error)
    }
}

async fn wait_for_exact_direct_child(
    parent_pid: u32,
    expected_executable: &Path,
) -> ExactProcessIdentity {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let output = Command::new("/bin/ps")
            .args(["-axo", "pid=,ppid="])
            .output()
            .unwrap();
        assert!(output.status.success());
        let mut matching = Vec::new();
        for row in String::from_utf8(output.stdout).unwrap().lines() {
            let fields = row.split_whitespace().collect::<Vec<_>>();
            if fields.len() != 2 || fields[1].parse::<u32>().ok() != Some(parent_pid) {
                continue;
            }
            let Some(pid) = fields[0].parse::<u32>().ok() else {
                continue;
            };
            if process_executable_path(pid).ok().as_deref() == Some(expected_executable) {
                matching.push(pid);
            }
        }
        if matching.len() == 1 {
            return ExactProcessIdentity::capture(matching[0], expected_executable);
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected one direct exact child of {parent_pid} executing {}, found {matching:?}",
            expected_executable.display()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn socket_identity(path: &Path) -> SocketIdentity {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    assert!(metadata.file_type().is_socket());
    SocketIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
    }
}

fn socket_identities_under(root: &Path) -> BTreeMap<PathBuf, SocketIdentity> {
    fn visit(current: &Path, sockets: &mut BTreeMap<PathBuf, SocketIdentity>) {
        for entry in std::fs::read_dir(current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let file_type = entry.file_type().unwrap();
            if file_type.is_dir() {
                visit(&path, sockets);
            } else if file_type.is_socket() {
                sockets.insert(path.clone(), socket_identity(&path));
            }
        }
    }

    let mut sockets = BTreeMap::new();
    visit(root, &mut sockets);
    sockets
}

struct DaemonGuard {
    child: Child,
    stderr_path: PathBuf,
    executable_path: PathBuf,
    process_start_identity: String,
    sidecar_identity: Option<ExactProcessIdentity>,
    sidecar_killed: bool,
    public_socket: PathBuf,
    public_socket_identity: Option<SocketIdentity>,
    private_socket_identities: BTreeMap<PathBuf, SocketIdentity>,
    runtime_parent: PathBuf,
    stopped: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SocketIdentity {
    device: u64,
    inode: u64,
    mode: u32,
}

impl DaemonGuard {
    async fn start(binaries: &CandidateBinaries, sandbox: &Sandbox) -> Self {
        Self::start_with_profile_policy(binaries, sandbox, true).await
    }

    async fn start_without_tested_profile(binaries: &CandidateBinaries, sandbox: &Sandbox) -> Self {
        Self::start_with_profile_policy(binaries, sandbox, false).await
    }

    async fn start_with_profile_policy(
        binaries: &CandidateBinaries,
        sandbox: &Sandbox,
        admit_test_profile: bool,
    ) -> Self {
        assert_eq!(binaries.pmuxd.parent(), Some(binaries.directory.as_path()));
        let (stderr_path, stderr) = (0_u32..=100)
            .find_map(|index| {
                let name = if index == 0 {
                    "pmuxd.stderr".to_owned()
                } else {
                    format!("pmuxd.stderr.{index}")
                };
                let path = sandbox.root_path.join(name);
                match OpenOptions::new().create_new(true).write(true).open(&path) {
                    Ok(file) => Some((path, file)),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(error) => panic!("failed to create exact daemon stderr file: {error}"),
                }
            })
            .expect("at most 101 daemon incarnations may share one E2E sandbox");
        let profile = serde_json::json!({
            "claude_version": PROFILE_VERSION,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "terminal_profile": "transparent",
            "input_transport": "sdk",
            "transcript_drain_ms": 50
        })
        .to_string();
        let mut command = Command::new(&binaries.pmuxd);
        command
            .arg("serve")
            .arg("--socket")
            .arg(&sandbox.public_socket)
            .arg("--rmuxd")
            .arg(&binaries.rmuxd)
            .arg("--launcher")
            .arg(&binaries.launcher)
            .arg("--runtime-parent")
            .arg(&sandbox.runtime_parent);
        if admit_test_profile {
            command.arg("--tested-claude-profile").arg(profile);
        }
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .unwrap();
        let pid = child.id();
        let process_start_identity = process_start_identity(pid)
            .unwrap()
            .expect("dedicated pmuxd has an exact process start identity");
        let executable_path = process_executable_path(pid).unwrap();
        assert_eq!(executable_path, binaries.pmuxd);
        let mut guard = Self {
            child,
            stderr_path,
            executable_path,
            process_start_identity,
            sidecar_identity: None,
            sidecar_killed: false,
            public_socket: sandbox.public_socket.clone(),
            public_socket_identity: None,
            private_socket_identities: BTreeMap::new(),
            runtime_parent: sandbox.runtime_parent.clone(),
            stopped: false,
        };

        let client = PmuxClient::new(&sandbox.public_socket).unwrap();
        for _ in 0..200 {
            if client.ping().await.is_ok() {
                guard.public_socket_identity = Some(socket_identity(&sandbox.public_socket));
                guard.private_socket_identities = socket_identities_under(&sandbox.runtime_parent);
                assert_eq!(
                    guard.private_socket_identities.len(),
                    2,
                    "private runtime must contain exact rmux and launcher sockets"
                );
                let socket_names = guard
                    .private_socket_identities
                    .keys()
                    .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
                    .collect::<BTreeSet<_>>();
                assert_eq!(socket_names, BTreeSet::from(["launcher.sock", "rmux.sock"]));
                guard.sidecar_identity =
                    Some(wait_for_exact_direct_child(guard.child.id(), &binaries.rmuxd).await);
                return guard;
            }
            if let Some(status) = guard.child.try_wait().unwrap() {
                let diagnostics = std::fs::read_to_string(&guard.stderr_path).unwrap_or_default();
                panic!("pmuxd exited during startup with {status}: {diagnostics}");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("pmuxd did not bind its public socket");
    }

    async fn stop(&mut self) {
        self.assert_runtime_identity();
        let pid = self.child.id();
        // SAFETY: this signal targets only the exact child created and retained
        // by this guard; no process lookup or broad match is used.
        let sent = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        assert_eq!(sent, 0, "failed to signal dedicated pmuxd");
        for _ in 0..400 {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "pmuxd shutdown failed: {status}");
                match process_start_identity(pid).unwrap() {
                    None => {}
                    Some(actual) if actual != self.process_start_identity => {}
                    Some(actual) => panic!(
                        "pmuxd pid {pid} with exact start identity {actual} survived shutdown"
                    ),
                }
                assert!(
                    !self.public_socket.exists(),
                    "exact public socket survived pmuxd shutdown"
                );
                self.stopped = true;
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("dedicated pmuxd did not stop within ten seconds");
    }

    /// Signals only the exact retained sidecar child, revalidating its
    /// identity first. Used for `SIGSTOP`/`SIGCONT`, which -- unlike
    /// `kill_exact_sidecar` -- leave the process alive and the socket bound,
    /// so nothing here waits for the endpoint to go away.
    fn signal_exact_sidecar(&self, signal: libc::c_int) {
        self.assert_runtime_identity();
        self.sidecar_identity
            .as_ref()
            .expect("private sidecar identity was captured")
            .signal(signal);
    }

    async fn kill_exact_sidecar(&mut self) {
        self.assert_runtime_identity();
        let sidecar = self
            .sidecar_identity
            .as_ref()
            .expect("private sidecar identity was captured")
            .clone();
        sidecar.signal(libc::SIGKILL);
        self.sidecar_killed = true;

        let rmux_socket = self
            .private_socket_identities
            .keys()
            .find(|path| path.file_name().and_then(|name| name.to_str()) == Some("rmux.sock"))
            .expect("private rmux socket identity was captured")
            .clone();
        for _ in 0..200 {
            if UnixStream::connect(&rmux_socket).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("exact killed sidecar retained a reachable rmux socket");
    }

    async fn stop_expecting_shutdown_failure(&mut self, sandbox: &Sandbox) {
        assert_eq!(self.public_socket, sandbox.public_socket);
        assert_eq!(self.runtime_parent, sandbox.runtime_parent);
        let daemon_identity = ExactProcessIdentity::capture(self.child.id(), &self.executable_path);
        assert_eq!(daemon_identity.start_identity, self.process_start_identity);
        daemon_identity.signal(libc::SIGTERM);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "faulted pmuxd did not stop within its bounded shutdown window"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        assert!(
            !status.success(),
            "an unconfirmed process escape or killed sidecar must fail daemon shutdown"
        );
        wait_for_exact_process_absence(&daemon_identity, Duration::from_secs(5)).await;
        if let Some(sidecar) = &self.sidecar_identity {
            wait_for_exact_process_absence(sidecar, Duration::from_secs(5)).await;
        }
        assert!(!self.public_socket.exists());
        assert_eq!(std::fs::read_dir(&self.runtime_parent).unwrap().count(), 0);
        self.stopped = true;
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn assert_identity(&self, binaries: &CandidateBinaries, sandbox: &Sandbox) {
        binaries.assert_unchanged();
        assert_eq!(self.executable_path, binaries.pmuxd);
        assert_eq!(self.public_socket, sandbox.public_socket);
        self.assert_runtime_identity();
    }

    fn assert_no_inet_sockets(&self) {
        assert_process_has_no_inet_socket(self.child.id(), "pmuxd");
        let sidecar = self
            .sidecar_identity
            .as_ref()
            .expect("private sidecar identity was captured");
        sidecar.assert_running();
        assert_process_has_no_inet_socket(sidecar.pid, "pmux-rmuxd");
    }

    fn assert_runtime_identity(&self) {
        let pid = self.child.id();
        assert_eq!(
            process_start_identity(pid).unwrap().as_deref(),
            Some(self.process_start_identity.as_str()),
            "dedicated pmuxd process identity changed during E2E"
        );
        assert_eq!(
            process_executable_path(pid).unwrap(),
            self.executable_path,
            "dedicated pmuxd does not execute the frozen candidate path"
        );
        assert_eq!(
            socket_identity(&self.public_socket),
            self.public_socket_identity
                .expect("public socket identity was captured after readiness"),
            "public socket was replaced during E2E"
        );
        for (path, expected) in &self.private_socket_identities {
            assert_eq!(
                socket_identity(path),
                *expected,
                "private runtime socket was replaced during E2E: {}",
                path.display()
            );
        }
        assert!(!self.sidecar_killed);
        self.sidecar_identity
            .as_ref()
            .expect("private sidecar identity was captured")
            .assert_running();
    }
}

#[cfg(target_os = "macos")]
fn assert_process_has_no_inet_socket(pid: u32, label: &str) {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &pid.to_string(), "-i"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        output.status.code() == Some(1) && stdout.trim().is_empty() && stderr.trim().is_empty(),
        "{label} pid {pid} exposed an unexpected Internet socket or could not be inspected:\nstdout={stdout}\nstderr={stderr}"
    );
}

#[cfg(target_os = "linux")]
fn assert_process_has_no_inet_socket(pid: u32, label: &str) {
    let mut process_socket_inodes = BTreeSet::new();
    for entry in std::fs::read_dir(format!("/proc/{pid}/fd")).unwrap() {
        let Ok(target) = std::fs::read_link(entry.unwrap().path()) else {
            continue;
        };
        let target = target.to_string_lossy();
        if let Some(inode) = target
            .strip_prefix("socket:[")
            .and_then(|value| value.strip_suffix(']'))
            .and_then(|value| value.parse::<u64>().ok())
        {
            process_socket_inodes.insert(inode);
        }
    }
    for table in ["tcp", "tcp6", "udp", "udp6"] {
        let contents = std::fs::read_to_string(format!("/proc/{pid}/net/{table}")).unwrap();
        for row in contents.lines().skip(1) {
            let fields = row.split_whitespace().collect::<Vec<_>>();
            let Some(inode) = fields.get(9).and_then(|value| value.parse::<u64>().ok()) else {
                continue;
            };
            assert!(
                !process_socket_inodes.contains(&inode),
                "{label} pid {pid} exposed an unexpected {table} socket inode {inode}"
            );
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn assert_process_has_no_inet_socket(_pid: u32, _label: &str) {}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

#[derive(Clone, Copy)]
struct ProcessResources {
    rss_kib: u64,
    open_fds: usize,
}

fn process_resources(pid: u32) -> ProcessResources {
    let output = Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .args(["-o", "rss="])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "failed to inspect dedicated pmuxd RSS"
    );
    let rss_kib = String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    ProcessResources {
        rss_kib,
        open_fds: exact_open_fd_count(pid),
    }
}

#[cfg(target_os = "linux")]
fn exact_open_fd_count(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .count()
}

#[cfg(target_os = "macos")]
fn exact_open_fd_count(pid: u32) -> usize {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid.to_string(), "-Fn"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "failed to inspect dedicated pmuxd descriptors"
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| line.starts_with('f'))
        .count()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exact_open_fd_count(_pid: u32) -> usize {
    0
}

fn runtime_entries(root: &Path) -> BTreeSet<PathBuf> {
    fn visit(root: &Path, current: &Path, entries: &mut BTreeSet<PathBuf>) {
        for entry in std::fs::read_dir(current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            entries.insert(path.strip_prefix(root).unwrap().to_path_buf());
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, entries);
            }
        }
    }

    let mut entries = BTreeSet::new();
    visit(root, root, &mut entries);
    entries
}
