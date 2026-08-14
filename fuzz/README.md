# Tracked fuzz targets

These targets are bounded Gate A searches for panic, hang, unsafe acceptance,
and state corruption at pmux's two untrusted parsing boundaries. They are not
a second transcript or protocol implementation:

- `transcript_jsonl` frames arbitrary bytes through the production transcript
  cursor, buffers unterminated suffixes, and stops on the first strict parser or
  engine error. Every accepted analysis is checked for outcome, usage, graph,
  and tool consistency.
- `transcript_cursor` mutates append, fragmentation, truncation, replacement,
  seek, and invalid-read sequences through the production cursor.
- `native_frame` drives the same incremental accumulator used by pmuxd across
  one-byte, uneven, and large fragments; compares partial and multi-frame
  outcomes across fragmentations; and feeds admitted payloads through the
  production protocol DTO deserializers. Every arbitrary payload is also
  wrapped in a valid frame, and the inclusive 8 MiB boundary is asserted.

The reviewed seed corpus is tracked below `corpus/`. A discovered crash or
semantic defect must be minimized and promoted to an ordinary Rust regression;
the generated `artifacts/` and evolving local corpus are evidence, not source.
[`../scripts/gate-a-fuzz.sh`](../scripts/gate-a-fuzz.sh) is the canonical
reproducible runner. It uses fixed target seeds, a fresh private corpus and
artifact directory for every invocation, verifies the pinned local cargo-fuzz
binary, separately runs fmt/check/clippy/test for this out-of-workspace package,
and hashes every source/corpus/tool input plus all evidence. Its default is the
normative 50,000 runs per target; `PMUX_FUZZ_RUNS` is only for clearly recorded
developer smoke runs. Per-target arguments, seeds, tool identities, and input
hashes are emitted into each private evidence directory;
[`../TESTING.md`](../TESTING.md) invokes only this canonical runner so a
hand-written command cannot silently weaken the gate.
