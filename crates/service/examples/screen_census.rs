//! Classifies every frame of a recorded screen corpus through the PRODUCTION
//! classifier and prints the census as JSON.
//!
//! This exists so the availability cost of the unrecognized-screen veto can be
//! MEASURED against real recordings rather than argued about. It reimplements
//! nothing: the verdict on every frame is
//! `pseudomux_service::driver_io::classify_terminal_snapshot`'s own, the
//! function the daemon that recorded those frames classified them with, and the
//! window it compares runs against is
//! `pseudomux_service::v1::UNRECOGNISED_SCREEN_VETO` itself.
//!
//! The census that matters is `longest_unrecognised_run_ms` per site. A count of
//! unrecognized frames says nothing on its own -- the veto fires on a
//! CONTINUOUS run, so one unrecognized frame between two ready ones costs
//! nothing and a run past the window costs the turn.
//!
//! ```text
//! cargo run --release -p pseudomux-service --example screen_census -- <dir> [<dir>...]
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use pseudomux_service::driver_io::classify_terminal_snapshot;
use pseudomux_service::screen_corpus::{Corpus, CorpusFrame};
use pseudomux_service::v1::UNRECOGNISED_SCREEN_VETO;

/// What one site's frames came out as, in recording order.
#[derive(Default)]
struct SiteCensus {
    verdicts: BTreeMap<&'static str, u64>,
    /// Longest continuous run of unrecognized frames, in frames and in
    /// milliseconds between the first and last frame of that run.
    longest_run_frames: u64,
    longest_run_ms: u64,
    open_run_frames: u64,
    open_run_started_ms: u64,
    last_ms: u64,
}

impl SiteCensus {
    fn observe(&mut self, verdict: &'static str, captured_unix_ms: u64) {
        *self.verdicts.entry(verdict).or_default() += 1;
        self.last_ms = captured_unix_ms;
        if verdict == "unrecognised" {
            if self.open_run_frames == 0 {
                self.open_run_started_ms = captured_unix_ms;
            }
            self.open_run_frames += 1;
            self.longest_run_frames = self.longest_run_frames.max(self.open_run_frames);
            self.longest_run_ms = self
                .longest_run_ms
                .max(captured_unix_ms.saturating_sub(self.open_run_started_ms));
        } else {
            self.open_run_frames = 0;
        }
    }
}

fn captured_unix_ms(frame: &CorpusFrame) -> u64 {
    match frame {
        CorpusFrame::Snapshot {
            captured_unix_ms, ..
        }
        | CorpusFrame::Styled {
            captured_unix_ms, ..
        } => *captured_unix_ms,
    }
}

fn main() {
    let directories: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if directories.is_empty() {
        eprintln!("usage: screen_census <corpus-dir> [<corpus-dir>...]");
        std::process::exit(2);
    }

    let mut corpora = Vec::new();
    for directory in &directories {
        match Corpus::load_dir(directory) {
            Ok(loaded) => corpora.extend(loaded),
            Err(error) => {
                eprintln!("{}: {error}", directory.display());
                std::process::exit(1);
            }
        }
    }

    let mut census: BTreeMap<String, SiteCensus> = BTreeMap::new();
    let mut files = Vec::new();
    let mut frames = 0_u64;
    for corpus in &corpora {
        files.push(serde_json::json!({
            "source": corpus.source.display().to_string(),
            "claude_version": corpus.stamp.claude_version,
            "os": corpus.stamp.os,
            "arch": corpus.stamp.arch,
            "label": corpus.stamp.label,
            "frames": corpus.frames.len(),
        }));
        // Runs are per file as well as per site: two recordings concatenated
        // are two sessions, and a run must never be measured across the seam.
        let mut in_file: BTreeMap<String, SiteCensus> = BTreeMap::new();
        for frame in &corpus.frames {
            frames += 1;
            // The verdict's own name for itself. This census does not own a
            // second spelling of the classifier's vocabulary: a variant added
            // to `TerminalScreenState` must appear in this output without
            // anybody editing this file.
            let verdict = classify_terminal_snapshot(&frame.to_terminal_snapshot()).label();
            in_file
                .entry(frame.site().to_owned())
                .or_default()
                .observe(verdict, captured_unix_ms(frame));
        }
        for (site, site_census) in in_file {
            let entry = census.entry(site).or_default();
            for (verdict, count) in site_census.verdicts {
                *entry.verdicts.entry(verdict).or_default() += count;
            }
            entry.longest_run_frames = entry.longest_run_frames.max(site_census.longest_run_frames);
            entry.longest_run_ms = entry.longest_run_ms.max(site_census.longest_run_ms);
        }
    }

    let veto_ms = u64::try_from(UNRECOGNISED_SCREEN_VETO.as_millis())
        .expect("the veto window is a millisecond count");
    let mut totals: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut by_site = serde_json::Map::new();
    let mut worst_run_ms = 0_u64;
    for (site, site_census) in &census {
        for (verdict, count) in &site_census.verdicts {
            *totals.entry(verdict).or_default() += count;
        }
        worst_run_ms = worst_run_ms.max(site_census.longest_run_ms);
        by_site.insert(
            site.clone(),
            serde_json::json!({
                "verdicts": site_census.verdicts,
                "longest_unrecognised_run_frames": site_census.longest_run_frames,
                "longest_unrecognised_run_ms": site_census.longest_run_ms,
            }),
        );
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "files": files,
            "frames": frames,
            "by_site": by_site,
            "totals": totals,
            "veto_window_ms": veto_ms,
            "longest_unrecognised_run_ms": worst_run_ms,
            "veto_would_have_fired": worst_run_ms >= veto_ms,
        }))
        .expect("a census of counted strings serializes")
    );
}
