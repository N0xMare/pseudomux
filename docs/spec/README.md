# pmux product specification

This book is the product contract. The root [README](../../README.md) is the
short landing page. How-to is the [user guide](../user-guide/README.md).
Engineering (pool internals, drain campaigns, defect log) is
[../engineering/README.md](../engineering/README.md).

| File | Contents |
| --- | --- |
| [00-overview.md](00-overview.md) | What pmux is and is not; two cells; position |
| [01-surfaces.md](01-surfaces.md) | Messages, ask, run, MCP, clients, ops |
| [02-operator.md](02-operator.md) | Daemon flags, compatibility, accounts |
| [03-cells.md](03-cells.md) | Minified vs Full, isolation, drain, `/clear` |
| [04-protocol.md](04-protocol.md) | UDS methods and refusals |
| [05-invariants.md](05-invariants.md) | Invariants and workspace map |

The thin [../spec.md](../spec.md) keeps the historical section numbers that
code comments still name (`§2`, `§3.1`, `§4`). New writing goes here.
