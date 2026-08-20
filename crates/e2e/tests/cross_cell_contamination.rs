#![cfg(unix)]

//! Private path table for minified-cell secret isolation.
//!
//! Compile-checked channel table (`ROOT_CHANNELS`). Live Path A
//! `start_session` sweeps that used to drive this table are historical:
//! public dispatch refuses `start_session`. Living product e2e is
//! `pool_concurrency.rs`. `tools/dev/check.sh --push` is the living invocation.

use std::collections::BTreeSet;
use std::path::Path;

/// One place a cell's private Claude configuration root can carry bytes.
///
/// `relative` is matched as a path prefix, so a row names either a single file
/// or a whole subtree without the caller saying which.
#[derive(Clone, Copy, Debug)]
struct Channel {
    name: &'static str,
    relative: &'static str,
}

/// Every channel a minified-cell isolation sweep must name.
///
/// `history.jsonl` is the row that motivates the table: Claude records EVERY
/// typed prompt there verbatim, it is append-only, `/clear` does not truncate
/// it (it appends `/clear` as a row of its own), and composer recall filters by
/// `project` -- the cwd -- and NOT by session, so it spans `/clear` by
/// construction.
const ROOT_CHANNELS: &[Channel] = &[
    Channel {
        name: "projects",
        relative: "projects",
    },
    Channel {
        name: "history",
        relative: "history.jsonl",
    },
    Channel {
        name: "paste-cache",
        relative: "paste-cache",
    },
    Channel {
        name: "shell-snapshots",
        relative: "shell-snapshots",
    },
    Channel {
        name: "todos",
        relative: "todos",
    },
    Channel {
        name: "file-history",
        relative: "file-history",
    },
    Channel {
        name: "backups",
        relative: "backups",
    },
    Channel {
        name: "global-config",
        relative: ".claude.json",
    },
    Channel {
        name: "user-settings",
        relative: "settings.json",
    },
    Channel {
        name: "statsig",
        relative: "statsig",
    },
    Channel {
        name: "ide",
        relative: "ide",
    },
    Channel {
        name: "plugins",
        relative: "plugins",
    },
    Channel {
        name: "shell-history",
        relative: "shell-history",
    },
    Channel {
        name: "logs",
        relative: "logs",
    },
    Channel {
        name: "cache",
        relative: "cache",
    },
    Channel {
        name: "sessions",
        relative: "sessions",
    },
    Channel {
        name: "credentials",
        relative: ".credentials.json",
    },
    Channel {
        name: "store-db",
        relative: "__store.db",
    },
];

/// Anything in a root that no row above claims.
const UNCLASSIFIED_CHANNEL: &str = "unclassified-root-entry";

fn channel_for(relative: &Path) -> &'static str {
    ROOT_CHANNELS
        .iter()
        .find(|channel| {
            let candidate = Path::new(channel.relative);
            relative == candidate || relative.starts_with(candidate)
        })
        .map_or(UNCLASSIFIED_CHANNEL, |channel| channel.name)
}

#[test]
fn every_named_channel_claims_the_paths_beneath_it_and_nothing_else() {
    assert_eq!(channel_for(Path::new("history.jsonl")), "history");
    assert_eq!(
        channel_for(Path::new("projects/pmux-e2e/x.jsonl")),
        "projects"
    );
    assert_eq!(channel_for(Path::new("paste-cache/a/b")), "paste-cache");
    assert_eq!(channel_for(Path::new(".credentials.json")), "credentials");
    assert_eq!(
        channel_for(Path::new("a-channel-claude-has-not-invented-yet/x")),
        UNCLASSIFIED_CHANNEL
    );
    let names = ROOT_CHANNELS
        .iter()
        .map(|channel| channel.name)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names.len(),
        ROOT_CHANNELS.len(),
        "channel names must be unique"
    );
}
