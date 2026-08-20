//! Mint-retention source pin, not a process/native-service lane.
//!
//! The Gate C/D cell runs this file. It pins that pool mint stays on
//! `start_session_owned` and registers the request's own `RetentionPolicy` —
//! there is no second retention decision. Public session refuse is
//! `Seam::build()` in `crates/service/src/native/tests/seam.rs` plus the
//! process_blackbox harness. This file is not that proof.

#[test]
fn start_session_owned_registers_the_request_retention() {
    const SOURCE: &str = include_str!("../src/native.rs");

    let body = |signature: &str| -> &str {
        let start = SOURCE
            .find(signature)
            .unwrap_or_else(|| panic!("{signature} is no longer in native.rs"));
        let tail = &SOURCE[start..];
        let end = tail
            .find("\n    }\n")
            .unwrap_or_else(|| panic!("{signature} has no closing brace at impl indentation"));
        &tail[..end]
    };

    let owned = body("pub(crate) async fn start_session_owned(");
    assert!(
        owned.contains("start_session_owned_with_retention"),
        "start_session_owned must forward to the one start path"
    );
    assert!(
        !owned.contains("ForcedOneShot") && !owned.contains("decide_retention"),
        "start_session_owned must not invent a retention"
    );

    let funnel = body("pub(crate) async fn start_session_owned_with_retention(");
    assert!(
        !funnel.contains("request.retention =")
            && !funnel.contains("decide_retention")
            && !funnel.contains("ForcedOneShot"),
        "the mint must register the request's own RetentionPolicy"
    );
}
