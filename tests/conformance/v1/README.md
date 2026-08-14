# pmux protocol-v1 client conformance vectors

These files are the language-neutral expectations consumed by the Rust,
TypeScript, and Python native-client tests. Rust protocol types remain the
authoritative executable specification; the vectors prevent the bindings from
silently choosing different validation, replay, or durable-ID behavior.

- `manifest.json` enumerates the closed v1 method, result, event, and error-code
  discriminants. Rust compares the error-code list through an exhaustive match.
  `value_enums` additionally pins every nested plain-string enum of the wire
  surface to its exact ordered value list. Rust checks it against serde output
  of exhaustive variant arrays, TypeScript against the `V1_VALUE_ENUMS` runtime
  arrays its unions are derived from, and Python against `typing.get_args` of
  the `Literal` aliases; each check fails in both directions, so adding a value
  in one language without the manifest, or the reverse, is a test failure.
  `tagged_unions` pins the enums serde tags *internally* -- the ones whose
  variants carry a payload, so the wire value is one key of an object rather
  than the whole of it -- to their discriminant key and their ordered variant
  list. Until this section existed those six were pinned by nothing: MEASURED,
  appending one payload-bearing variant to each of `ConfigSource`,
  `LifecycleMode`, `MessageBlock`, `RetentionPolicy`, `SessionIdentity` and
  `SystemPromptPolicy` left `cargo test -p pseudomux-protocol` green six times
  out of six, and both shipped clients throw on a `MessageBlock` kind they do
  not recognize. **In all three languages the variant list is derived, not
  typed:** Rust constructs one sample per variant behind a wildcard-free
  `match` and asks `serde_json` what each spells; TypeScript ties its arrays to
  the union types with `satisfies`, so `tsc` refuses a variant present in one
  and not the other; Python reads the `Literal` discriminant off each
  `TypedDict` member of the alias. The *discriminant key* is derived too, in
  Rust and Python, as the only key every variant carries with a string value no
  other variant repeats. Rust additionally scans its own sources, so a seventh
  internally-tagged `pub enum` that never reaches this file is a test failure
  rather than a silent hole.
- `golden.json` contains one complete request/result pair for every method,
  one complete frame for every event, one error frame, and the UUIDv5
  durable-attempt inputs and outputs. Rust, TypeScript, and Python consume this
  exact file; a client's generated request UUID is checked and normalized only
  for comparison with the fixed golden UUID. **Both "every method" and "every
  event" are compared to `manifest.methods` and `manifest.events` by name, in
  all three languages, and never to a literal.** The method half used to be
  three hand-written copies of `11`, and MEASURED, the corpus covered eleven of
  twelve: `run_stateless` -- the whole of Path B and the only producer of
  `stateless_result` -- had no pair in any language while both shipped clients
  implemented and validated it. The event half stayed a hand-written `14` in the
  same file and the same commit that derived the method count, and neither
  client asserted event coverage at all; MEASURED, appending `"future_event"` to
  `manifest.events` left every golden test in all three languages green. A
  literal freezes the corpus at the size it had the day it was typed: deleting
  an entry reddens it, failing to add one does not, which is exactly how an
  *appended* method or event slips through.
- `cases.json` contains focused error-body, replay-gap, canonical UUID,
  safe-integer, non-standard JSON, and all-client negative cases. It also owns
  the complete strict-request object-pointer inventory, every required
  result/event/error field deletion, and the reserved turn-lease cases. A
  reserved lease is a valid request DTO that the v1 service rejects before
  side effects with non-retryable `unsupported_feature`; clients must not
  reject it while encoding. Negative response frames use the literal
  `$REQUEST_ID` only where the fake peer must echo the request UUID generated
  by the client under test.

Every Rust, TypeScript, and Python client recursively injects one additive
future field at each object boundary in every complete result, event, and error
frame from `golden.json`. The traversal includes the document root and nested
objects and mutates exactly one boundary per decode, so an accepting outer
object cannot mask a strict nested decoder.

UUID validation accepts the RFC textual hex alphabet case-insensitively but
requires the exact `8-4-4-4-12` hyphenated form. UUID comparison is
case-insensitive; serializers may emit canonical lowercase spelling.

Protocol-owned integers use the exact nonnegative range
`0..=9_007_199_254_740_991`. Integer-valued numbers recursively nested in
opaque JSON use the signed range
`-9_007_199_254_740_991..=9_007_199_254_740_991`. Nonfinite numbers are never
valid JSON. Every client rejects values outside these domains before sending
or accepting a v1 frame.
