# pseudomux-claude

Pure Claude Code transcript semantics for pmux. This crate does not launch
Claude, watch the filesystem, or inspect a terminal.

The integration boundary has two stages:

1. A platform tailer supplies `FileMetadata` and offset-addressed byte ranges to
   `TranscriptCursor`. Only newline-terminated `CompleteLine` values may be
   passed onward. Replacement and truncation reset cursor generation and partial
   state.
2. `JsonlParser` preserves the complete source object while classifying known
   records. `TranscriptEngine` is armed after historical EOF, acknowledges the
   exact typed prompt, selects the newest main `parentUuid` branch, groups
   assistant fragments, and derives a conservative terminal candidate.

Strict mode is the production default. Unknown data on the selected semantic
chain, malformed known fields, conflicting logical-message snapshots, and
ambiguous grouping identities are errors. Unknown off-branch records are
preserved and reported as warnings. Terminal quietness, hook delivery, and
filesystem stability remain service-layer completion gates and are deliberately
outside this crate.

Run the standalone checks with:

```sh
cargo test --manifest-path crates/claude/Cargo.toml
cargo clippy --manifest-path crates/claude/Cargo.toml --all-targets -- -D warnings
```
