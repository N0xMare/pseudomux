# pmux product specification

The contract is the book under [spec/](spec/README.md). This file keeps the
section numbers that code comments still name. How-to is
[user-guide/](user-guide/README.md).

## 1. What pmux is

See [spec/00-overview.md](spec/00-overview.md).

## 2. What pmux is not

See [spec/00-overview.md](spec/00-overview.md). Interactive session methods
and `claude -p` are not product surfaces (`session_surface_removed`).

## 3. Caller surfaces

See [spec/01-surfaces.md](spec/01-surfaces.md).

### 3.1 Messages (harness contract)

Pin, release, class. Loopback only.

### 3.2 `run_stateless` / `pmux ask`

Minified one-shot. Caller names no resource.

### 3.3 `run_stateful` / `pmux run`

Full cell. `--cwd` required. `--stateful` required. Skip-permissions for
unattended use.

### 3.4 Ops

`ping` and `doctor` start no turn.

### 3.5 Clients

TypeScript, Rust, Python: Messages + `run_stateless` + `run_stateful`.
MCP advertises `run_stateless` only.

## 4. Operator daemon

See [spec/02-operator.md](spec/02-operator.md). `--pool-parent` enables the
pool. Argv for a minified mint is built by pmux, not by the caller.

## 5. Compatibility

See [spec/02-operator.md](spec/02-operator.md). Promoted cells: macos
2.1.258 through 2.1.280 and linux/x86_64 2.1.227 through 2.1.280, drain 250 ms. linux/aarch64 is 2.1.272 through 2.1.280, drain 500 ms.

## 6. Transport

See [spec/04-protocol.md](spec/04-protocol.md). Public methods: `ping`,
`diagnose`, `run_stateless`, `run_stateful`.

## 7. Invariants

See [spec/05-invariants.md](spec/05-invariants.md). Transcript is authority.

## 8. Workspace components (product)

See [spec/05-invariants.md](spec/05-invariants.md).
