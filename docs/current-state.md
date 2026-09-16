# current-state.md

**Position.** The product contract is [spec/00-overview.md](spec/00-overview.md).
How-to is [user-guide/](user-guide/README.md). This file is no longer a
third normative document.

Shipped: minified pool + Messages + `pmux ask` / `run_stateless`; opt-in
Full `pmux run` / `run_stateful` behind `pmuxd --stateful`. Promoted Claude
Code 2.1.258 through 2.1.272 on macos/aarch64 and 2.1.227 through 2.1.272
on linux/x86_64, both `transcript_drain_ms` 250.

The 3,618-line 2026-08 essay is [archive/current-state-2026-08.md](archive/current-state-2026-08.md).
The pre-squash commit ledger is [engineering/defect-log.md](engineering/defect-log.md)
(also linked as `docs/defect-log.md`). Dated Path B receipts stay in
[engineering/](engineering/README.md).

### 9.4 Post-commit findings tombstone (C6)

Closed. The living record is the spec overview. Historical rows remain in
`docs/archive/current-state-2026-08.md` §9.4.

### 9.29 THE BUG CLASS, instance thirty-three — a reordering "verified gate-equivalent" for one property, and a revision that was never a mutation counter

Closed as a living heading. The ordinal is pinned here so protocol comments
that name this instance keep resolving. The write-up is
`docs/archive/current-state-2026-08.md`.
