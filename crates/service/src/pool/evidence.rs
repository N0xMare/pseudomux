//! Retained Path B evidence: the drain corpus for the NEXT Claude Code version.
//!
//! # The problem this exists for
//!
//! `docs/version-drift.md` sec.2.2 measured where pmux's own compatibility
//! evidence came from and got an uncomfortable answer: **178 of the 186
//! reachable post-answer arrivals at 2.1.220 came out of pmux's own paid
//! campaign directories** under `~/pmux-drain-campaigns` and `/private/tmp`.
//! Only 8 came from anywhere else on that host. The 2.1.220 profile could be
//! built "for free" only because a Gate B campaign had just been run at
//! 2.1.220; the transcripts were free, the turns that wrote them were not.
//!
//! And sec.2.1 is why re-analysis cannot substitute: the `turn_duration` marker
//! is a `cli`-entrypoint feature, ZERO of the corpus's 169,237 versioned SDK
//! rows carry one, and `cli` is 1.04% of them. At a brand-new Claude Code
//! version there are simply **no `cli` turns to re-analyse** --
//! `measure_transcript_drain.py --version 2.1.226 --bound-ms 1000` exits 5,
//! "nothing to check", on the host that shipped 2.1.226.
//!
//! A pmux Path B cell IS a `cli` cell. So every ordinary Path B turn already
//! writes exactly the evidence a promotion needs, and then the pool erases it
//! four lines later. This module keeps a redacted copy instead, so version
//! N+1's corpus accumulates BEFORE promotion is needed.
//!
//! # What is retained, and what is not
//!
//! **Not the transcript.** A mirror of it pruned to [`RETAINED_ROW_FIELDS`] --
//! eight keys, none of which can hold a prompt or a completion. That is not a
//! judgement call about which fields look safe: it is exactly the set
//! `tools/promotion/measure_transcript_drain.py` reads, published there as
//! `FIELDS_READ`, which that tool PRUNES every row to before any measurement.
//! `tests::the_retained_fields_are_the_ones_the_measurement_tool_reads` reads
//! the Python file and fails if the two sets differ.
//!
//! The consequence is worth stating plainly: the retained corpus is not an
//! approximation of a transcript corpus for this purpose, it is an exact one.
//! The tool cannot tell the difference, because the tool never reads a field
//! that was dropped.
//!
//! # Where it writes, and how it is bounded
//!
//! `<socket parent>/pool-evidence/`, beside `logs/`, derived
//! through the same `daemon_sibling_dir` those two are, and owner-only at every
//! level pmux creates. It is deliberately NOT under the pool parent: that tree
//! is erased per instance and its containment rules exist to make that safe.
//!
//! Bounded by [`MAX_EVIDENCE_BYTES`], enforced after each write by deleting the
//! oldest files until the directory is under budget. `--pool-no-evidence`
//! turns it off entirely and `--pool-evidence-dir` moves it.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::private_dir::create_private_dir_all;

/// Every row field the retained mirror keeps, and the whole of it.
///
/// DERIVED from `tools/promotion/measure_transcript_drain.py`'s `FIELDS_READ`,
/// not chosen: the measurement is the only consumer, and a field it does not
/// read is a field pmux would be retaining for no stated reason. Sorted,
/// because the check that binds the two compares sets and a sorted array reads
/// as one.
///
/// Nothing here can carry text a caller wrote or a model produced. `message`,
/// `content`, `toolUseResult`, `summary`, `lastPrompt` and `title` are all
/// absent, and they are absent because nothing measures them, which is a much
/// stronger reason than "they looked sensitive".
pub const RETAINED_ROW_FIELDS: [&str; 8] = [
    "entrypoint",
    "isMeta",
    "isSidechain",
    "promptId",
    "subtype",
    "timestamp",
    "type",
    "version",
];

/// The size budget for the whole retained tree.
///
/// CHOSEN against a measurement rather than picked. Mirroring the 189
/// transcripts behind `evidence/promoted-profile-2.1.220-macos-aarch64.json`
/// through exactly this field set produced **271,497 bytes**, i.e. **1,437
/// bytes per transcript** -- and `measure_transcript_drain.py` over those
/// mirrors reproduced that receipt's `post_answer_arrivals`,
/// `recommended_transcript_drain_ms`, `full_drain_binds_on` and turn count
/// identically. So 64 MiB is on the order of **46,000 retained transcripts**,
/// two orders of magnitude past the 425 that produced the shipped pooled bound,
/// for 64 MiB of disk.
///
/// It is a HARD ceiling and not a target: retention deletes oldest-first until
/// the tree is under it, so the property is "the tree never exceeds this",
/// never "the tree is about this big".
pub const MAX_EVIDENCE_BYTES: u64 = 64 * 1024 * 1024;

/// What one instance's teardown contributed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retained {
    /// Transcripts mirrored.
    pub files: usize,
    /// Rows kept across them.
    pub rows: usize,
    /// Files deleted to stay under [`MAX_EVIDENCE_BYTES`].
    pub pruned: usize,
}

/// One JSONL row, pruned to [`RETAINED_ROW_FIELDS`].
///
/// `None` when nothing survives, which is how a row that carried only content
/// leaves no trace at all rather than an empty object.
#[must_use]
pub fn retained_row(line: &str) -> Option<String> {
    let row: Value = serde_json::from_str(line).ok()?;
    let row = row.as_object()?;
    let mut kept = Map::new();
    // Iterating the ALLOWLIST and not the row: a row key that merely resembles
    // one of these -- `type_`, `Timestamp`, a nested `message.type` -- is not
    // in the allowlist and is therefore not consulted at all.
    for field in RETAINED_ROW_FIELDS {
        if let Some(value) = row.get(field) {
            kept.insert(field.to_owned(), value.clone());
        }
    }
    if kept.is_empty() {
        return None;
    }
    serde_json::to_string(&Value::Object(kept)).ok()
}

/// Mirror every transcript under one instance's config root into `into`.
///
/// Called from the pool's teardown AFTER the process is proven reaped and
/// BEFORE the tree is erased -- the only window in which the file exists and
/// nothing is writing to it.
///
/// # Errors
///
/// Only a failure to create the destination. A transcript that cannot be read
/// is skipped: retention is evidence-gathering, and a teardown that failed
/// because a log could not be copied would trade a guarantee for a
/// convenience.
pub fn retain_instance_transcripts(config_root: &Path, into: &Path) -> io::Result<Retained> {
    let sources = transcripts_under(config_root);
    if sources.is_empty() {
        return Ok(Retained::default());
    }
    create_private_dir_all(into)?;

    let mut retained = Retained::default();
    for source in sources {
        let Ok(text) = std::fs::read_to_string(&source) else {
            continue;
        };
        let mut mirrored = String::new();
        let mut rows = 0_usize;
        for line in text.lines() {
            if let Some(kept) = retained_row(line.trim()) {
                mirrored.push_str(&kept);
                mirrored.push('\n');
                rows += 1;
            }
        }
        if rows == 0 {
            continue;
        }
        let Some(name) = source.file_name() else {
            continue;
        };
        if write_private(&into.join(name), &mirrored).is_ok() {
            retained.files += 1;
            retained.rows += rows;
        }
    }

    retained.pruned = prune(into, MAX_EVIDENCE_BYTES);
    Ok(retained)
}

/// `<config_root>/projects/<slug>/<session>.jsonl`, which is the one shape
/// Claude Code writes and the one `pseudomux_claude::locator` reads.
fn transcripts_under(config_root: &Path) -> Vec<PathBuf> {
    let projects = config_root.join("projects");
    let Ok(slugs) = std::fs::read_dir(&projects) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for slug in slugs.flatten() {
        let Ok(entries) = std::fs::read_dir(slug.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
                && entry.file_type().is_ok_and(|kind| kind.is_file())
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn write_private(path: &Path, contents: &str) -> io::Result<()> {
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Delete oldest-first until the tree is under `budget`. Returns how many went.
///
/// Oldest by MODIFICATION TIME and not by name: the file name is a session
/// uuid, which carries no order at all, and a name-ordered prune would delete
/// an arbitrary file rather than the least useful one.
fn prune(into: &Path, budget: u64) -> usize {
    let Ok(entries) = std::fs::read_dir(into) else {
        return 0;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = Vec::new();
    let mut total = 0_u64;
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
        total = total.saturating_add(metadata.len());
        files.push((modified, metadata.len(), entry.path()));
    }
    files.sort();
    let mut pruned = 0;
    for (_, length, path) in files {
        if total <= budget {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(length);
            pruned += 1;
        }
    }
    pruned
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set one file's mtime to an exact second, so a test can state which file
    /// is older instead of hoping.
    fn set_mtime_seconds(path: &Path, seconds: i64) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let raw = CString::new(path.as_os_str().as_bytes()).unwrap();
        let times = [
            libc::timespec {
                tv_sec: seconds as libc::time_t,
                tv_nsec: 0,
            },
            libc::timespec {
                tv_sec: seconds as libc::time_t,
                tv_nsec: 0,
            },
        ];
        // SAFETY: `raw` is a NUL-terminated path this test just created and
        // `times` is a two-element array of the shape `utimensat` requires.
        #[allow(unsafe_code)]
        let outcome = unsafe { libc::utimensat(libc::AT_FDCWD, raw.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(outcome, 0, "could not set an mtime on {}", path.display());
    }

    fn write_transcript(root: &Path, session: &str, rows: &[Value]) {
        let project = root.join("projects").join("-tmp-pmux-pool-0-0-cwd");
        std::fs::create_dir_all(&project).unwrap();
        let mut text = String::new();
        for row in rows {
            text.push_str(&serde_json::to_string(row).unwrap());
            text.push('\n');
        }
        std::fs::write(project.join(format!("{session}.jsonl")), text).unwrap();
    }

    fn turn_rows(secret: &str) -> Vec<Value> {
        vec![
            serde_json::json!({
                "type": "user", "promptId": "p1", "isMeta": false, "isSidechain": false,
                "entrypoint": "cli", "version": "2.1.226",
                "timestamp": "2026-08-09T10:00:00.000Z",
                "message": {"role": "user", "content": secret},
            }),
            serde_json::json!({
                "type": "assistant", "version": "2.1.226", "entrypoint": "cli",
                "timestamp": "2026-08-09T10:00:01.000Z",
                "message": {"role": "assistant", "content": [{"type": "text", "text": secret}]},
            }),
            serde_json::json!({
                "type": "system", "subtype": "turn_duration", "version": "2.1.226",
                "entrypoint": "cli", "timestamp": "2026-08-09T10:00:01.200Z",
                "durationMs": 1200, "lastPrompt": secret,
            }),
        ]
    }

    /// **The set is DERIVED from the measurement tool, in the tool's own file.**
    ///
    /// This is the whole reason the retained mirror is defensible. Choosing
    /// eight fields that "look safe" is a judgement nobody can check; taking
    /// the eight the only consumer reads is a fact, and this reads that file to
    /// establish it. A field added to `FIELDS_READ` without being retained
    /// makes the corpus silently useless for whatever needed it; a field
    /// retained without being read is pmux keeping data for no stated reason,
    /// which is exactly what a redaction policy exists to prevent.
    #[test]
    fn the_retained_fields_are_the_ones_the_measurement_tool_reads() {
        let tool = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/promotion/measure_transcript_drain.py")
            .canonicalize()
            .expect("the measurement tool is part of the repository");
        let source = std::fs::read_to_string(&tool).expect("the tool is readable");
        let (_, after) = source
            .split_once("FIELDS_READ = (")
            .expect("the tool publishes the fields it reads as FIELDS_READ");
        let (body, _) = after
            .split_once(')')
            .expect("FIELDS_READ is a parenthesised tuple");
        let mut declared: Vec<String> = body
            .split(',')
            .map(|item| item.trim().trim_matches('"').to_owned())
            .filter(|item| !item.is_empty())
            .collect();
        declared.sort();
        let mut retained: Vec<String> = RETAINED_ROW_FIELDS
            .iter()
            .map(|f| (*f).to_owned())
            .collect();
        retained.sort();
        assert_eq!(
            retained,
            declared,
            "RETAINED_ROW_FIELDS and {}'s FIELDS_READ disagree",
            tool.display()
        );
        assert!(
            declared.len() >= 4,
            "the tool claims to read {} field(s), which is not enough to find a turn at all",
            declared.len()
        );
    }

    /// A mirror carries the timings and the row kinds, and carries NO text a
    /// caller wrote or a model produced.
    #[test]
    fn a_mirror_keeps_the_measurement_and_none_of_the_content() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let into = temp.path().join("evidence");
        let secret = "the caller's private prompt about their salary";
        write_transcript(
            &root,
            "11111111-1111-1111-1111-111111111111",
            &turn_rows(secret),
        );

        let retained = retain_instance_transcripts(&root, &into).unwrap();
        assert_eq!(retained.files, 1);
        assert_eq!(retained.rows, 3);

        let mirrored =
            std::fs::read_to_string(into.join("11111111-1111-1111-1111-111111111111.jsonl"))
                .unwrap();
        assert!(
            !mirrored.contains(secret),
            "the mirror reproduced the caller's text: {mirrored}"
        );
        for absent in ["message", "durationMs", "lastPrompt", "content"] {
            assert!(
                !mirrored.contains(absent),
                "the mirror kept `{absent}`, which nothing measures: {mirrored}"
            );
        }
        // ...and it kept everything the measurement needs: a turn start, a
        // terminal candidate, the marker after it, the version and the
        // entrypoint that make the turn countable at all.
        for owed in [
            "\"promptId\"",
            "\"assistant\"",
            "\"turn_duration\"",
            "\"2.1.226\"",
            "\"cli\"",
            "2026-08-09T10:00:01.200Z",
        ] {
            assert!(
                mirrored.contains(owed),
                "the mirror dropped {owed}, which the drain measurement reads: {mirrored}"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(into.join("11111111-1111-1111-1111-111111111111.jsonl"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "a retained transcript is owner-only");
            let dir = std::fs::metadata(&into).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir, 0o700, "the retention directory is owner-only");
        }
    }

    /// A row whose only keys are content leaves NO trace, rather than an empty
    /// object that a reader would count as a row.
    #[test]
    fn a_row_that_is_only_content_is_dropped_entirely() {
        assert_eq!(retained_row(r#"{"message":{"content":"secret"}}"#), None);
        assert_eq!(retained_row("not json"), None);
        assert_eq!(retained_row("[]"), None);
        // A key that merely resembles an allowlisted one is not allowlisted.
        assert_eq!(retained_row(r#"{"Type":"user","type_":"x"}"#), None);
    }

    /// The tree is BOUNDED, and the bound is enforced by deleting the oldest.
    #[test]
    fn the_retained_tree_never_exceeds_its_budget() {
        let temp = tempfile::tempdir().unwrap();
        let into = temp.path().join("evidence");
        create_private_dir_all(&into).unwrap();
        let body = "x".repeat(4096);
        let mut newest = String::new();
        for index in 0..16 {
            let path = into.join(format!("{index:02}.jsonl"));
            std::fs::write(&path, &body).unwrap();
            // Distinct mtimes, set EXPLICITLY. Writing sixteen files in a loop
            // and trusting them to land on different timestamps makes the test
            // depend on the filesystem's clock resolution, which on APFS is
            // fine and on a 1-second-granularity filesystem silently turns the
            // ordering assertion below into a coin flip.
            set_mtime_seconds(&path, 1_000 + i64::from(index));
            newest = format!("{index:02}.jsonl");
        }
        let budget = 4096 * 4;
        let pruned = prune(&into, budget);
        assert_eq!(
            pruned, 12,
            "the prune must delete oldest-first until under budget"
        );
        let total: u64 = std::fs::read_dir(&into)
            .unwrap()
            .flatten()
            .map(|entry| entry.metadata().unwrap().len())
            .sum();
        assert!(
            total <= budget,
            "the retained tree is {total} bytes against a {budget} byte budget"
        );
        assert!(
            into.join(&newest).exists(),
            "the newest evidence is the evidence a promotion needs, and it was deleted"
        );
        assert!(
            !into.join("00.jsonl").exists(),
            "the oldest evidence survived a prune that deleted 12 files"
        );
    }

    /// An instance that never wrote a transcript contributes nothing and
    /// creates nothing.
    #[test]
    fn an_instance_with_no_transcript_leaves_no_directory_behind() {
        let temp = tempfile::tempdir().unwrap();
        let into = temp.path().join("evidence");
        assert_eq!(
            retain_instance_transcripts(&temp.path().join("root"), &into).unwrap(),
            Retained::default()
        );
        assert!(
            !into.exists(),
            "retention created a directory with nothing in it"
        );
    }

    /// Only a regular file named `*.jsonl` under a project slug is a
    /// transcript, and both halves of that sentence are one `&&`.
    ///
    /// The same shape as `driver_io`'s rotation scan, on the path that COPIES
    /// what it finds into the evidence tree. Read as `||`, every ordinary file
    /// beside a transcript -- and every directory whose name ends `.jsonl` --
    /// becomes something retention opens, redacts and republishes under
    /// `<evidence>/`. That is a disclosure boundary, not a best-effort one: the
    /// evidence tree is what a promotion reads.
    #[test]
    fn only_a_regular_file_named_jsonl_under_a_slug_is_a_transcript() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let slug = root.join("projects").join("-tmp-pmux-pool-0-0-cwd");
        std::fs::create_dir_all(&slug).unwrap();
        let transcript = slug.join("session.jsonl");
        std::fs::write(&transcript, "").unwrap();
        std::fs::write(slug.join("notes.txt"), "").unwrap();
        std::fs::write(slug.join("jsonl"), "").unwrap();
        std::fs::create_dir(slug.join("nested.jsonl")).unwrap();
        assert_eq!(transcripts_under(&root), vec![transcript]);
    }
}
