# Quickstart

Rust 1.88+, a Unix host, and an installed Claude Code binary.

```bash
cargo build --workspace --release
```

Start a daemon with explicit owner-only paths. **Give it `--pool-parent`
and `--pool-claude`.** Without them every `pmux ask` is refused with
`unsupported_feature`. Full `pmux run` also needs `--stateful`:

```bash
RUNTIME_DIR="$PWD/.context/pmux-dev"
mkdir -p "$RUNTIME_DIR"
chmod 700 "$RUNTIME_DIR"
SOCKET="$RUNTIME_DIR/pmux.sock"

target/release/pmuxd serve \
  --socket "$SOCKET" \
  --runtime-parent "$RUNTIME_DIR" \
  --pool-parent "$RUNTIME_DIR/pool" \
  --pool-claude "$(command -v claude)" \
  --stateful
```

`--pool-claude` must be absolute. The binary's version must be in the
[promoted table](../spec/02-operator.md) for this OS/arch, or you pass
`--tested-claude-profile`. macos PATH `claude` 2.1.280 is inside the macos
cell. linux/x86_64 admits through 2.1.280; linux/aarch64 admits 2.1.272 through 2.1.280.

```bash
export PMUX_SOCKET="$SOCKET"
target/release/pmux ping
target/release/pmux doctor --claude "$(command -v claude)"
target/release/pmux ask --model claude-sonnet-5 -- 'Reply with exactly OK'
```

`doctor`'s pool layer tells you whether the pool is configured.
