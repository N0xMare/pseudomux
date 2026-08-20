# Docs

**To use pmux, read the root [README](../README.md) and [spec.md](spec.md).**
Pi setup is [examples/pi](../examples/pi/README.md).

The product is a local API over a warm pool of constrained embedded Claude
Code processes. TypeScript, Rust, and Python clients plus `pmux run` /
`ping` / `doctor` are the caller surface.

Everything else in this directory is engineering: compatibility receipts,
test ownership, and dated measurements. It is not a second product.

| If you are… | Read |
| --- | --- |
| Calling the API or wiring a harness | Root README, then [spec.md](spec.md), then [examples](../examples/README.md) |
| Checking the tree, pinning Claude, or dropping `--tested-claude-profile` | [tools/dev](../tools/dev/README.md) |
| Promoting a Claude Code version (historical receipts) | [version-drift.md](version-drift.md) |
| Reading the historical freeze census (Gate A is gone) | [testing.md](testing.md) |
| Looking up where the project stands | [current-state.md](current-state.md) |

Dated receipts and `docs/archive/` are not updated to stay true.
