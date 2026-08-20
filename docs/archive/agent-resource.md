# The agent resource

**Status: BUILT, with four deviations recorded in §9.** This document was the build input. The tree
now carries the resource; `docs/spec.md` §4.8 is the amended, authoritative statement of what shipped
and `docs/current-state.md` §10 carries its invariant. Every path:line citation below is to code as
it stood at `c57136d`, before the build, and several have moved.

---

## 0. The decision, and the one it contradicts

### 0.1 Not a Messages-API surface

The Anthropic Messages API is stateless with **client-held history**: the caller sends the full
`messages[]` array on every call, and the server holds nothing between calls.

pmux cannot honour that on either path.

* **Path B** is stateless with *no history at all*. `Request::ClearSession`
  (`crates/protocol/src/v1.rs:495`) is driven between turns and the entire isolation argument depends
  on the transcript being abandoned — `crates/protocol/src/v1.rs:2457-2462` states the mechanic. A
  `messages[]` array would have to be replayed into a cell whose defining property is that nothing
  survives the previous turn.
* **Path A** history lives in a real Claude Code TUI and its JSONL transcript. The transcript is the
  sole completion authority (`docs/spec.md` §6.4), and it is append-only, produced by a process pmux
  drives rather than a document pmux composes. It cannot be reconstructed from an array.

A `messages[]` parameter would therefore promise continuity neither path can deliver. That is the bug
class at the API layer: a parameter whose name promises more than its implementation tests.

### 0.2 The Anthropic surface that *does* match

**Managed Agents.** A persisted, versioned Agent config (`POST /v1/agents`); Sessions that reference
an agent id and pin a version; events sent and streamed. pmux already has sessions
(`Request::StartSession`), turns (`Request::RunTurn`), and an event stream
(`Request::SubscribeEvents`). What it lacks is the **agent-as-resource split**: today every
`start_session` re-specifies the entire launch configuration —
`crates/protocol/src/v1.rs:1338-1379` is eleven fields, of which eight are launch policy a given
caller retypes identically on every call (§1.1 classifies all eleven).

### 0.3 The contradiction, stated plainly

`docs/spec.md:664` says:

> pmux has no server-side agent registry and MUST NOT grow one.

That is a current, argued invariant of this product, and it is §4.8's entire subject
(`docs/spec.md:662-731`). **This design contradicts it.** I am not going to pretend otherwise, and I
would not build this without the owner explicitly retiring or amending §4.8. See §8 for the exact
decisions I am blocked on.

§4.8 rests on two arguments. One survives this design; one does not, and the difference is where the
work is.

**Argument A — "the daemon and its clients run as the same uid (§10.2), so a server-side registry
adds zero enforcement: anything it would refuse, the caller can send directly as an ordinary DTO."**

This survives intact, and this design **concedes it completely**. An agent is not a security
boundary and must never be documented as one. Everything in §1's containment rules is a *narrowing*
of what one request may say, not a *capability* the caller could not otherwise reach — the caller can
always send the inline DTO instead. The value of the resource is deduplication, pinning, and
auditability, and if this document ever reads as though an agent *constrains* a caller, that sentence
is wrong. §6 refuses several otherwise-attractive features precisely because they would only make
sense if Argument A were false.

**Argument B — "child argv MUST remain a pure function of the request the daemon received (§4.4). A
registry makes it a function of the request *and* of server-held state, so one DTO could produce two
different launches."**

This is the load-bearing one, and it is answerable — but only by a specific shape:

1. The reference is **pinned by version**, and the version is **named in the request**
   (`AgentRef::version` is required — §3.4). "Latest at start time" is refused, because that makes
   the launch a function of *when the request arrived*, which is exactly the impurity §4.4 forbids.
2. A stored version is **immutable**. An update mints a new version and never mutates an old one
   (§2.3). So `(agent_id, version)` denotes one byte-string for all time.
3. Resolution is a **pure function** run once at admission: `(AgentSpec, per-session fields) ->
   StartSessionRequest`. Everything downstream — `resolve_claude_launch`
   (`crates/service/src/claude_launch.rs:131`), `admit_bound_resources`
   (`crates/service/src/native.rs:3439`) — receives a DTO indistinguishable from one a caller typed
   inline.
4. The resolved DTO is **echoed on the response**, with a `config_digest` over its unredacted bytes,
   so a caller can check what it actually launched rather than trust that resolution did what it said.

With those four, §4.4 becomes *literally* true rather than approximately true: argv is a pure function
of `(the request, an immutable value the request names)`. That is a weaker claim than §4.4's current
one and the difference must be written into §4.4 rather than glossed. Without version pinning it is
false and I would not build it.

**One thing §4.8 gets exactly right and this design keeps: "evidence admission belongs to the
operator; preferences belong to the caller" (`docs/spec.md:688-690`).** An agent carries preferences.
It carries no evidence. It cannot claim its cell is tested, cannot widen the launch-environment
allowlist (`crates/protocol/src/v1/launch_environment.rs`), cannot admit an untested compatibility
profile that `CompatibilityProfileRegistry` would refuse, and cannot reach anything in
`PoolSettings` (`crates/service/src/pool/config.rs`).

---

## 1. The resource

### 1.1 The central invariant

> **An agent may narrow what a session may name. It may never name a resource on the session's
> behalf.**

Every field classifies mechanically under it, and the classification is the design:

| Class | Rule | Fields |
|---|---|---|
| **Launch policy** | No filesystem or process identity. Moves to the agent. | `claude` (all of `ClaudeLaunchConfig`), `auth_policy`, `terminal`, `lifecycle`, `retention`, `compatibility`, `cell`, `environment.set`, `environment.unset` |
| **Bound resource** | Names a directory or identity pmux *claims*. Stays per-session; the agent may bound it. | `cwd`, `config_isolation`, `identity` |
| **Caller process snapshot** | Is a fact about the calling process at call time. Stays per-session, structurally. | `environment.snapshot` |

### 1.2 Why `cwd` stays per-session — and what the agent may do instead

This is the field the brief says to think hard about, and the leak family is the reason.

A cwd is not a preference. `LiveResourceClaim::directories`
(`crates/service/src/native.rs:3393-3399`) enumerates it as one of exactly two directories a live
session *binds*, and the comment above it records why the enumeration exists: "leak 7's third shape
was an intruder cwd standing on a live cell's CONFIGURATION ROOT, which the old per-field comparisons
could not express." `admit_cwd` (`crates/service/src/native.rs:3792`) states the rest: "A cwd is
where the transcript slug comes from and where the file is." And the client-side profile already
refuses to carry one, for a reason `docs/spec.md:695-700` writes out: "`cwd` is the most consequential
launch parameter; a config file that silently redirects where an agent operates is exactly the
ambient resolution this product refuses everywhere else."

So an agent supplying a cwd is out. But there is a strictly better move available, and it is the one
this design takes:

**An agent may carry `containment.workspace_root`: an absolute directory every session's `cwd` must
resolve inside. The agent never supplies a cwd; it only bounds one.**

Three properties make this safe where supplying would not be:

1. **The caller still writes the cwd on every call.** No ambient resolution; the command a caller
   typed still contains the directory it will operate in.
2. **It is narrowing-only, and composed with `AND`.** Containment is an *additional* refusal. It runs
   *before* `admit_bound_resources`, which then runs unchanged. There is no value of
   `workspace_root` that makes an otherwise-refused cwd admissible. This is the rule that keeps it a
   predicate rather than a capability, and §7 pins it with a test whose whole job is to prove the
   composition direction.
3. **The predicate is the existing one, not a fresh `starts_with`.** It must route through
   `claude_launch::one_directory_contains_the_other`
   (`crates/service/src/claude_launch.rs:388`), whose own doc records why: it "replaces
   `root.starts_with(cwd) || cwd.starts_with(root)`", which is wrong under symlinks and under the
   `/tmp` → `/private/tmp` rewrite this host performs (`crates/service/src/claude_launch.rs:397`).
   A second containment predicate is a second answer to drift from the first.

`config_isolation` gets the same treatment for the same reason, plus one of its own: the root is a
resource with a **seed disposition** (`crates/service/src/config_isolation.rs:104-111`), so two live
sessions naming the same root are already a governed collision. An agent that *named* a root would
make an agent id into a contention key — N sessions from one agent would all claim one root, and the
`SeedDisposition::VerifyOnly` path would start refusing starts as a function of how popular the agent
is. So the agent carries `containment.require_config_isolation: bool`: a session started from this
agent **must** name a pmux-owned root. It still does not say which.

`identity` stays per-session because it *is* the session's name.

### 1.3 Why `environment.snapshot` cannot be stored

`EnvironmentSpec::snapshot` (`crates/protocol/src/v1.rs:1571-1572`) is "a complete caller snapshot".
A stored snapshot is a stale snapshot the moment the caller's shell changes, and it is a file full of
environment *values* at rest.

The design therefore does **not** reuse `EnvironmentSpec` on the agent with a note asking callers not
to set `snapshot`. That note is the bug class. It introduces a new type with the field deleted:

```rust
/// The environment policy an agent carries. Deliberately NOT [`EnvironmentSpec`].
///
/// `EnvironmentSpec::snapshot` is a fact about the calling process at call time,
/// and there is no version of "an agent stores one" that is not either stale or
/// a file of environment values at rest. Reusing `EnvironmentSpec` here and
/// documenting that `snapshot` must be empty would be a rule enforced by prose;
/// deleting the field makes the sentence unsayable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEnvironmentSpec {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub unset: BTreeSet<String>,
}
```

`set` still carries values, and those values can be secrets — `--env KEY=VALUE` is documented as the
channel that "bypasses the launch allowlist" (`bin/pmux/src/cli.rs:432-437`). That is why the store is
owner-only from birth (§2.2) and why `get_agent` returns values as digests (§3.5).

### 1.4 Versioning

CMA-shaped, with two deliberate divergences.

**A monotonic counter, not a timestamp.** CMA versions are numeric timestamps. pmux must not copy
that: two updates inside one clock tick share a timestamp, and a clock that steps backwards orders
them wrongly. `AgentVersion` is a `u64` counter starting at 1. **Ordering** is the counter;
**identity** is `config_digest`, a SHA-256 over the canonical serialization of the stored `AgentSpec`
(`sha2` is already a workspace dependency, `Cargo.toml:41`). Two versions with equal digests are the
same configuration, which is what a caller actually wants to compare.

**The update fence is required, not optional.** CMA makes `version` optional on update: supply it for
optimistic concurrency, omit it for last-write-wins. pmux should make `expected_version` **required**,
because this repo already settled this exact question twice and both times chose a mandatory fence:
`generation_id` on every session-addressed request, and
`ClearSessionRequest::expected_transcript_session_id`
(`crates/protocol/src/v1.rs:2469-2492`), whose doc is the argument verbatim — an optional fence is a
fence a caller forgets, and the recovery path costs "one round trip and never a wrong answer." A stale
fence is `ErrorCode::IdConflict`, which is precisely what that code already means at
`crates/service/src/v1/actor.rs:1097` ("does not match the active attachment") and not what
`IdCollision` means (`crates/service/src/native.rs:1313`, "already active").

**A running session is unaffected by any update.** A session resolves and *copies* its
`AgentSpec` at start and pins `(agent_id, version, config_digest)` for life. This is the same rule,
for the same reason, as `SessionCell` — `crates/protocol/src/v1.rs:1381-1386`: "chosen once at start
and there is deliberately no request that changes it mid-session, because a cell change mid-flight
would mean a turn could finish on a proof it did not start under." Pinning by *value* is also why
§6 can refuse a `delete` method without stranding anyone: a running session never reads the file again.

---

## 2. Storage

### 2.1 Location

```
<agent-store>/
  <agent-id>/               0700   # canonical hyphenated UUID
    head                    0600   # a LOWER BOUND on the newest version, one line
    1.json                  0600   # immutable
    2.json                  0600   # immutable
```

`<agent-store>` defaults to `<socket-dir>/agents`, beside `logs/` — which is exactly how
`daemon_log_dir` already derives its own path (`bin/pmuxd/src/main.rs:849-855`). An explicit
`pmuxd serve --agent-store DIR` overrides it, because an operator who moves `--socket` should not
silently lose their agents.

### 2.2 The privacy bar, and where it is already written

The store must not be the weak link. It is held to **the same bar as the socket directory and the
Path B pool parent**, which are already held to one definition:

* `ensure_private_directory` (`bin/pmuxd/src/main.rs:940-973`) — creates privately, and **refuses**
  a directory that is not owner-only and owned by the caller, naming the reason. Never
  re-permissions.
* `require_private_parent` (`crates/service/src/pool/mod.rs:1388-1420`) — the pool parent, held to
  "the same bar the daemon's socket directory is held to."
* `create_private_dir_all` (`crates/service/src/private_dir.rs:64`) — every level pmux creates is
  `0700` **from birth**, passed to `mkdir(2)` rather than chmod'd afterwards, so the directory is
  never observable at a wider mode (`crates/service/src/private_dir.rs:33-41`).

The agent store adds one thing those three do not need: **files**. Files are created with
`OpenOptions::mode(0o600)` for the identical reason directories use `DirBuilderExt::mode` — `umask`
can only clear bits, and `0600` has no group or other bits to clear, so the file is never observable
at a wider mode and there is no create-then-chmod window.

**Per-read mode check.** An agent file is read at `start_session` time, which can be long after boot.
A file an operator widened between boots is a file whose contents pmux should not trust, so the read
path re-checks `mode & 0o077 == 0` and refuses. The client-side loader already does exactly this for
profile files (`crates/client/src/agent_profile.rs:478-486`); this is the same bar, server side. And
as everywhere else: **refuse, never re-permission.**

### 2.3 Write discipline

Per-version files rather than one file with a history array, so a torn write can only lose the newest
version and can never corrupt one a caller pinned.

1. Write `<version>.json.tmp.<pid>.<seq>.<nanos>`, created `0600`, under a name **no other writer
   shares** — a name that was a pure function of the destination let a second writer truncate the
   first's bytes mid-write (§9.20's `trailing characters at line 1 column 1497`).
2. `sync_all` the file. On Apple targets that is `fcntl(F_FULLFSYNC)`, not `fsync(2)`.
3. **`link(2)`** the finished inode to `<version>.json`. Not `rename` — naming the file and refusing
   to replace one must be the SAME syscall, or two concurrent writers on one fence both publish.
4. `fsync` the directory, **best-effort**: the failure is discarded, because publication is already
   atomic against a reader and this is not a reason to fail a write that landed.
5. Rewrite `head` by the tmp+rename dance. `rename` is right *here* and only here, because replacing
   is exactly what advancing a pointer means.

An existing `<version>.json` is **never** opened for write. §7 pins that with a test that mutates the
writer to truncate-in-place and requires it to redden.

**Steps 3 and 5 are two files and cannot be one syscall**, so a crash between them is recovered by
the next reader rather than narrowed away. `head` is therefore a durable LOWER BOUND, and the newest
published version is derived by walking forward from it over every version NAME that exists — a loop,
not a one-step lookahead, because step 5 writes an absolute value with no lock and a descheduled
writer can make the pointer regress. The step predicate is step 3's, exactly: any name, not "a
readable version", so the number a later `update_agent` mints is one no name is taken for. A version
published before the pointer reached it is **ADOPTED**, whose caller-visible consequence is that an
update interrupted by a crash MAY have landed — see `docs/spec.md` §4.8.2 and
`docs/current-state.md` §9.21 for why that beats discarding it and for the 19-of-45 measurement that
forced the question.

### 2.4 The id is a UUID, and this is not cosmetic

The agent id is a daemon-minted UUID v4, canonical hyphenated on the wire (already enforced by
`deserialize_canonical_uuid`, `crates/protocol/src/v1.rs:370-383`) and used verbatim as the directory
name. `name` is a human label with no filesystem role and no uniqueness requirement.

**Do not reuse `agent_profile::validate_agent_name` as a path-component validator.** MEASURED against
its own predicate (`crates/client/src/agent_profile.rs:640-653`), it admits every one of these:

```
      ".." admitted=true
       "." admitted=true
     "..." admitted=true
    "a..b" admitted=true
       "-" admitted=true
```

That is harmless today, because the name is a JSON map key. It is a directory traversal the moment it
becomes a path component. Minting a UUID makes traversal **unconstructible** rather than filtered,
which is the same move `CONFIG_ROOT_ENV_DOORS` makes for the config-root environment names —
"the whole point is that the DOOR is deleted... so there is no spelling left to get wrong"
(`crates/service/src/claude_launch.rs:60-64`).

---

## 3. The wire

Conventions taken from `crates/protocol/src/v1.rs`: `deny_unknown_fields` on requests, permissive
results, `safe_u64`/`optional_safe_u64` for integers, `skip_serializing_if` where absent means
default, `Box` on large results.

### 3.1 Variants append last, never reorder

`crates/protocol/src/v1.rs:492-494` states the rule and the reason: the shared conformance manifest
compares these lists **positionally**, and appending "never renumbers a position a reader has already
memorised."

```rust
pub enum Request {
    // ... twelve existing variants, untouched, in order ...
    RunStateless(RunStatelessRequest),
    // Appended below, in this order.
    CreateAgent(CreateAgentRequest),
    GetAgent(GetAgentRequest),
    ListAgents(ListAgentsRequest),
    UpdateAgent(UpdateAgentRequest),
}

pub enum ResponseResult {
    // ... twelve existing variants, untouched, in order ...
    StatelessResult(Box<StatelessResult>),
    // Appended below, in this order. Each is boxed for the reason
    // `SessionSnapshot` and `TurnResult` are (v1.rs:611-615, 619-623):
    // an `AgentDescriptor` carries a whole `ClaudeLaunchConfig` plus an
    // environment map, and no cheap response should pay for that on every move.
    AgentCreated(Box<AgentDescriptor>),
    Agent(Box<AgentDescriptor>),
    AgentList(Box<AgentList>),
    AgentUpdated(Box<AgentDescriptor>),
}
```

**Four result variants, not one shared `Agent`.** This is forced, and the constraint is worth stating
because it is not obvious: the golden corpus asserts that each method's result type is **distinct**
(`crates/protocol/tests/v1_golden.rs:544-551` inserts into a `BTreeSet` and asserts the insert
succeeded). `create_agent` and `get_agent` both answering `agent` would redden it. Collapsing them
would mean changing that invariant, which is a worse trade than three extra variants.

### 3.2 The stored configuration

```rust
pub type AgentId = Uuid;

/// Monotonic revision of one agent's stored configuration, starting at 1.
///
/// A counter and deliberately not a timestamp: two updates inside one clock tick
/// would share a timestamp, and a clock that steps backwards would order them
/// wrongly. This field ORDERS. Identity is [`AgentDescriptor::config_digest`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AgentVersion(u64);

impl<'de> Deserialize<'de> for AgentVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Same idiom as `SessionGenerationId` (v1.rs:413-420): the newtype owns
        // its own domain check so no caller has to remember one.
        let value = safe_u64::deserialize(deserializer)?;
        if value == 0 {
            return Err(serde::de::Error::custom("agent version starts at 1"));
        }
        Ok(Self(value))
    }
}

/// Everything an agent stores. The complete difference between this and
/// `StartSessionRequest` is §1.1's table, and that is the design.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    /// A human label. No filesystem role, no uniqueness requirement, and
    /// deliberately not the id: see §2.4.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub claude: ClaudeLaunchConfig,
    #[serde(default)]
    pub environment: AgentEnvironmentSpec,
    #[serde(default)]
    pub auth_policy: AuthPolicy,
    #[serde(default)]
    pub terminal: TerminalSpec,
    #[serde(default)]
    pub lifecycle: LifecycleMode,
    #[serde(default)]
    pub retention: RetentionPolicy,
    #[serde(default)]
    pub compatibility: CompatibilityPolicy,
    #[serde(default, skip_serializing_if = "SessionCell::is_default")]
    pub cell: SessionCell,
    #[serde(default)]
    pub containment: AgentContainment,
}

/// What an agent may say about the resources a session names.
///
/// Every field here NARROWS. There is no value of any of them that makes an
/// otherwise-refused start admissible; each is composed with `AND` against the
/// checks that already run. See §1.2.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentContainment {
    /// Absolute directory every session's `cwd` must resolve INSIDE.
    ///
    /// The agent never supplies a cwd; the caller still writes one on every
    /// call. Tested with `claude_launch::one_directory_contains_the_other` and
    /// never with `starts_with`, for the reason that function's own doc gives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    /// Whether a session from this agent MUST name a `config_isolation` root.
    /// It does not name one, and cannot: see §1.2.
    ///
    /// `false` is REFUSED when `cell` is `minified`, rather than silently
    /// overridden by the minified cell's own requirement. A field that is
    /// accepted and ignored is instance twenty of the bug class
    /// (`docs/current-state.md` §9.13); CMA ships one of exactly this shape
    /// (an `effort` inside a per-session model override is accepted and not
    /// applied), and pmux must not copy it.
    #[serde(default)]
    pub require_config_isolation: bool,
}
```

### 3.3 Requests and results

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateAgentRequest {
    pub spec: AgentSpec,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetAgentRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub agent_id: AgentId,
    /// Omit for the current head. Absent means "whatever is current NOW", which
    /// is honest for a read and is exactly what `AgentRef` refuses for a launch:
    /// a read reports, a launch commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<AgentVersion>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListAgentsRequest {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateAgentRequest {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub agent_id: AgentId,
    /// The version the caller believes is current. REQUIRED, and a fence rather
    /// than a routing key -- the same idiom and the same argument as
    /// `ClearSessionRequest::expected_transcript_session_id` (v1.rs:2469-2492).
    /// Any value that is not the current head is `ErrorCode::IdConflict`,
    /// including one stale by exactly one revision, and nothing here is ever
    /// answered as "your update already landed".
    pub expected_version: AgentVersion,
    /// The COMPLETE replacement spec. There is deliberately no partial update:
    /// a patch surface has one merge rule per field and no test derives the
    /// list. Read, edit, write.
    pub spec: AgentSpec,
}

/// One stored agent version, as read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDescriptor {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub agent_id: AgentId,
    pub version: AgentVersion,
    /// Lowercase hex SHA-256 over the canonical serialization of the UNREDACTED
    /// spec. This is identity; `version` is only order. It is also what makes
    /// §0.3's argv-purity claim checkable without disclosing an environment
    /// value or an inline settings document.
    pub config_digest: String,
    /// Environment values and inline settings/MCP documents are replaced by
    /// digests: see §3.5.
    pub spec: AgentSpec,
    #[serde(with = "safe_u64")]
    pub created_at_ms: TimestampMs,
    #[serde(with = "safe_u64")]
    pub updated_at_ms: TimestampMs,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentList {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentSummary>,
}

/// Deliberately not `Vec<AgentDescriptor>`: a list is a directory read, and
/// returning every agent's full spec would make `list` the most expensive
/// request on the socket and would spray every stored environment key across
/// one frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub agent_id: AgentId,
    pub version: AgentVersion,
    pub config_digest: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub cell: SessionCell,
    #[serde(with = "safe_u64")]
    pub updated_at_ms: TimestampMs,
}
```

### 3.4 The session reference

```rust
/// Appended to `StartSessionRequest`.
///
/// `skip_serializing_if` for the same reason `cell` has it (v1.rs:1371-1377):
/// request DTOs are `deny_unknown_fields`, so a daemon that predates this field
/// REFUSES any request carrying it. Omitting it from the wire when absent keeps
/// every pre-existing caller's bytes identical.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub agent: Option<AgentRef>,

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRef {
    #[serde(deserialize_with = "deserialize_canonical_uuid")]
    pub agent_id: AgentId,
    /// The exact stored version this session runs. REQUIRED.
    ///
    /// There is deliberately no "omit for latest" shorthand, which CMA offers.
    /// "Latest at start time" makes the launch a function of WHEN the request
    /// arrived, which is precisely the impurity `docs/spec.md` §4.4 forbids and
    /// §4.8 objects to. It is also the same refusal `RunStatelessRequest::model`
    /// already makes and for the same stated reason (v1.rs:2557-2561): an absent
    /// value would silently resolve against whatever the daemon happens to hold.
    /// A caller wanting the head does one `get_agent` and gets a value it can log.
    pub version: AgentVersion,
}
```

`StartSessionRequest::claude` becomes `Option<ClaudeLaunchConfig>` with
`skip_serializing_if = "Option::is_none"`. A present value serializes byte-identically, so every
existing golden request is unchanged.

### 3.5 Redaction

`get_agent` and `list_agents` never emit an environment value, an inline settings document, or an
inline MCP document. Each is replaced by `sha256:<hex>` of its bytes. This mirrors what `pmux probe`
already promises — "Environment values, inline settings and MCP documents, and the system prompt are
never printed" (`bin/pmux/src/cli.rs:286-289`) — and it is why `config_digest` exists: the digest is
computed over the *unredacted* spec, so it still identifies the configuration exactly while the frame
discloses nothing.

**`system_prompt` is NOT redacted.** `probe` redacts it because `probe` prints to a terminal; an
agent's system prompt is the single most important thing about it and an inspection surface that
hides it is useless. This is a deliberate divergence and it is stated so nobody "fixes" it later.

### 3.6 The both-modes refusal, derived

Exactly one of `agent` or the inline launch fields may be present. Never both.

**Merging is refused, and CMA is the argument.** CMA offers `agent_with_overrides`, and its own
documentation records that an `effort` inside a per-session `model` override is accepted and silently
not applied — "the one field where the override form silently does nothing rather than erroring." A
merge surface has one silently-ignored-field risk per field pair and nothing derives the list. That is
instance twenty of this repo's bug class, shipped in the reference API. pmux refuses instead.

**The check is derived, not hand-listed.** Hand-listing is how the pool census listed six of seven
constructors (`docs/current-state.md` §9.12). The derivation reuses this repo's own stated technique —
`validate_v1_serializable` is "deliberately serializer-backed rather than a second field inventory"
(`crates/protocol/src/v1.rs:306-319`):

1. Serialize a fully-populated `AgentSpec` and collect its **leaf paths**.
2. Serialize the incoming `StartSessionRequest` and collect its leaf paths.
3. Refuse with `InvalidConfig` if `agent` is present and the intersection is non-empty, naming the
   first colliding path.

`environment.snapshot` survives automatically because it is a leaf of `StartSessionRequest` and not of
`AgentSpec` — no exception list, which is the point. `name`, `description`, and `containment` are
leaves of `AgentSpec` only and cannot collide.

**The structural alternative was considered and rejected.** A tagged
`enum LaunchSource { Inline {...}, Agent {...} }` would make "both" *unrepresentable* rather than
refused, which this repo prefers where it is available. It is not available here: it moves `claude`
from the top level into `launch.claude` and breaks every existing caller's bytes. The brief is
explicit that inline must keep working, so flat-plus-derived-refusal is the answer.

### 3.7 Error codes: none are new

**Zero new `ErrorCode` variants.** `crates/protocol/src/v1.rs:2927-2962` is a closed list that both
shipped clients hard-reject unknown members of — TypeScript at
`clients/typescript/src/client.ts:309-311` ("has an unknown discriminant"), Python at
`clients/python/pmux_client/client.py:1074-1076`. Adding one is a breaking change forcing both
encoders to move in the same release, and none of these conditions needs one:

| Condition | Code | Why it is honest |
|---|---|---|
| Malformed `AgentSpec`; both-modes conflict; containment violation; `require_config_isolation: false` with `cell: minified` | `InvalidConfig` | It is a configuration that is invalid. |
| No such agent, or no such version | `InvalidConfig` + `recommendation` | The caller's launch configuration references something that does not exist. The advice channel names the fix. |
| `expected_version` is not the head | `IdConflict` | Exactly what that code already means: a fence that does not match current state (`crates/service/src/v1/actor.rs:1097`). |
| Store unreadable, or widened since boot | `InvalidConfig` | Same code and same shape as `require_private_parent`'s refusal (`crates/service/src/pool/mod.rs:1409`). |

Every refusal writes the actionable half to `details.recommendation` — the channel
`bin/pmux/src/main.rs:65-67` defines and renders, and the one `docs/current-state.md` §9.14 records
as the settled answer to "the daemon knew the answer and never printed it." For a missing agent that
reads `no agent <id>; list them with 'pmux agent list'`.

**If an owner insists on `AgentNotFound`**, the cost is: one manifest entry, one entry in
`PMUX_ERROR_CODES` (`clients/typescript/src/protocol.ts:547`), one in `KNOWN_ERROR_CODES`
(`clients/python/pmux_client/client.py:61`), one arm in `error_code_name`
(`crates/protocol/tests/v1_conformance_vectors.rs:79`), and a lockstep three-language release.
I recommend against it.

### 3.8 Conformance-vector impact — and a precondition that is a live defect

**Required additions:**

* `tests/conformance/v1/manifest.json` — 4 methods appended to `methods`, 4 to `results`. Both lists
  are compared positionally against exhaustive `match`es
  (`crates/protocol/tests/v1_conformance_vectors.rs:148-196`), so the Rust side is a compile error
  until the `wire_tags!` arms are added. That half is already derived and already safe.
* `tests/conformance/v1/golden.json` — 4 new `requests_and_results` entries.
* `tests/conformance/v1/cases.json` — 4 `strict_request_object_pointers` entries and 4
  `client_required_field_deletions.results` entries, plus the reviewed pointer-count literal at
  `crates/protocol/tests/v1_golden.rs:578-583`.

**The precondition. `golden.json` does not cover the surface it claims to cover, and three
hand-written literals are why.** This is instance twenty-one of the bug class, and it is in the
shared corpus rather than in anything this design adds.

`tests/conformance/v1/README.md:16` claims:

> `golden.json` contains one complete request/result pair for every method,

MEASURED against the manifest it is pinned to:

```
golden methods: ['ping', 'start_session', 'run_turn', 'cancel_turn', 'inspect_session',
                 'attach_session', 'close_session', 'subscribe_events', 'run_once',
                 'clear_session', 'diagnose']
MISSING from golden: ['run_stateless']
```

Eleven of twelve. `run_stateless` — the whole of Path B, the method `pmux ask` reaches and the only
producer of `StatelessResult` — has no golden pair in any of the three languages, so it is the one
method/result pair that no byte-exact cross-language frame pins. Both clients *implement* it
(`clients/typescript/src/client.ts:1377`, `clients/python/pmux_client/client.py:311`) and both
*validate* the result (`clients/python/pmux_client/client.py:1130`), against no shared vector.

The guard cannot see it, because it compares the corpus to a number rather than to the surface:

```
crates/protocol/tests/v1_golden.rs:520      assert_eq!(golden.requests_and_results.len(), 11);
crates/protocol/tests/v1_golden.rs:553      assert_eq!(methods.len(), 11);
crates/protocol/tests/v1_golden.rs:554      assert_eq!(results.len(), 11);
clients/typescript/tests/golden-conformance.test.mjs:214   assert.equal(GOLDEN.requests_and_results.length, 11);
clients/python/tests/test_golden_conformance.py:224        self.assertEqual(len(GOLDEN["requests_and_results"]), 11)
```

Three languages, three hand-written copies of `11`, none derived from `manifest.methods`. The literal
freezes the corpus at the size it had the day it was written: a method *appended* to `Request` adds a
manifest entry and a positional variant, and nothing requires a golden pair to follow. Deleting an
entry reddens it; failing to add one does not. All 8 golden tests are green today:

```
$ cargo test -p pseudomux-protocol --test v1_golden
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
```

This is the same defect the manifest test already fixed for itself, in the same directory. Its own doc
records the history (`crates/protocol/tests/v1_conformance_vectors.rs:126-135`): the manifest check
"passed with the manifest three methods short of the surface, because the 'closed v1 surface' it
compared against was a copy of the manifest with a different syntax," and the fix was the `wire_tags!`
macro's exhaustive `match`. **The fix was applied to `manifest.json`'s checker and not to
`golden.json`'s.**

**Therefore: derive the count before adding any agent method.** Replace all three literals with
`manifest.methods.len()`, and add the missing `run_stateless` golden pair. If this is not done first,
appending four methods produces a corpus covering 11 of 16 with every test green, and the design in
this document will have shipped its own twenty-second instance.

I did not fix this. It touches the shared corpus and all three client suites, it is not the agent
resource, and it wants its own commit with its own mutation proof per language.

---

## 4. The migration

### 4.1 Inline keeps working, byte-for-byte

* `agent` is `Option<AgentRef>` with `skip_serializing_if`, so a caller that omits it emits the exact
  bytes it emits today — the same argument `cell` makes at `crates/protocol/src/v1.rs:1371-1377`.
* `claude` becomes `Option<ClaudeLaunchConfig>` with `skip_serializing_if`; a present value
  serializes identically, so the `start_session` golden request is unchanged.
* No existing field is renamed, reordered, retyped, or given a new default.
* `run_once` embeds `StartSessionRequest` (`crates/protocol/src/v1.rs:2760`) and therefore inherits
  agent support with no separate work — and inherits §3.6's refusal too.

### 4.2 Which direction the sugar runs

The brief asks whether inline eventually becomes sugar for an anonymous agent. **No — and the honest
reading is the reverse.**

An anonymous agent is a stored object nobody named. It needs a lifetime rule, which needs a
garbage-collection sweep, which is a new background failure mode on the admission path, all to make
one code path *look* like the other.

The real relationship is the other way around: **an agent reference is sugar for an inline config.**
Resolution is a pure function that produces exactly the DTO the caller would have typed, and
everything downstream sees a request it cannot distinguish from an inline one. That is not an
implementation convenience — it is what keeps `docs/spec.md` §4.4 literally true, and §7 pins it with
an equality assertion rather than a description.

### 4.3 The client-side profile does not go away

`crates/client/src/agent_profile.rs` stays, and stays client-side. It is now an **authoring** tool:
`extends` chains, composition operators, and `require_env` are how a human writes a spec, and the
output is an `AgentSpec` that `pmux agent create` uploads. Server-side inheritance is refused in §6.

`docs/spec.md` §4.8 must be rewritten rather than deleted: everything it says about *profiles*
remains true, and its "MUST NOT grow one" clause becomes the amended §0.3 argument.

---

## 5. Three surfaces, one vocabulary

### 5.1 Protocol

`create_agent`, `get_agent`, `list_agents`, `update_agent`.

### 5.2 CLI

```
pmux agent create --spec-file <FILE>            # or --profile/--profile-file to author from a profile
pmux agent list
pmux agent get    <AGENT_ID> [--version <N>]
pmux agent update <AGENT_ID> --expected-version <N> --spec-file <FILE>

pmux start   --agent <AGENT_ID> --agent-version <N> --cwd <DIR>
pmux run     --agent <AGENT_ID> --agent-version <N> --cwd <DIR> [PROMPT]
pmux turn|clear|attach|close|inspect             # unchanged; they address a session, not an agent
pmux ask                                         # unchanged. See §6.1.
```

**There is a name collision and it must be resolved, not papered over.** `--agent` and `--agent-file`
already exist and already mean the *client-side profile* (`bin/pmux/src/cli.rs:342-350`, with
`PMUX_AGENT` / `PMUX_AGENT_FILE`). Two different things cannot both be `--agent`.

The resolution: **the profile flags are renamed to `--profile` / `--profile-file`
(`PMUX_PROFILE` / `PMUX_PROFILE_FILE`), and `--agent` becomes the server agent.** The old spellings
are **refused with a message naming the new one**, never silently aliased — a silent alias is exactly
how a caller reaches for one feature and gets the other. This is a user-visible CLI break and is one
of the decisions in §8.

Rejected: `--agent` accepting either a UUID or a profile name, dispatching on string shape. That is
ambient resolution by syntax, which this product refuses everywhere.

### 5.3 MCP

Tool names match the protocol method names exactly, as every existing tool does
(`bin/pmux-mcp/src/tools.rs:169-239`). **Descriptions are read by models, so each says what it does
*and what it refuses*** — the standard `run_stateless`'s description already sets
(`bin/pmux-mcp/src/tools.rs:219-237`).

* **`create_agent`** — "Store one reusable Claude launch configuration and return its id and version
  1. REFUSES a spec naming `cwd`, `config_isolation`, session identity, a prompt, or an environment
  snapshot: those are per-session and are named on every `start_session`. An agent may only *narrow*
  what a session names — `containment.workspace_root` bounds the cwd a session may use and does not
  supply one. Creates nothing on the filesystem a caller can name."
* **`get_agent`** — "Read one stored agent version. Omit `version` for the current head. Environment
  values and inline settings/MCP documents are returned as `sha256:` digests and never in the clear;
  `config_digest` still identifies the configuration exactly."
* **`list_agents`** — "List every stored agent's id, current version, digest, name, and cell.
  Deliberately does NOT return full specs — use `get_agent` for one."
* **`update_agent`** — "Store a new immutable version of one agent and return it. `expected_version`
  is REQUIRED and is a fence: any value that is not the current head is refused with `id_conflict`,
  including one stale by exactly one revision, and no update is ever answered as 'already landed'.
  `spec` is a COMPLETE replacement, not a patch. Running sessions are unaffected — they pinned their
  version at start."
* **`start_session`** (amended) — "…Supply EITHER `agent` (an id and an exact version) OR the inline
  launch fields, never both; a request carrying both is refused with `invalid_config` naming the
  colliding field. `cwd` is always required and is never taken from the agent."

`every_enum_in_every_tool_schema_names_exactly_its_protocol_variants`
(`bin/pmux-mcp/src/tools.rs:1162`) walks every schema for `"enum"` arrays and checks each against the
variants parsed out of `crates/protocol/src/v1.rs`, in both directions. The new schemas are covered by
it automatically, which is the point of it being derived.

---

## 6. What I would not build

### 6.1 Path B must not gain an agent reference. This is the strongest refusal here.

`RunStatelessRequest`'s own doc is the argument, and it is already written
(`crates/protocol/src/v1.rs:2538-2547`):

> It names no resource. There is no session id, no generation, no turn id, no cwd, no config root, no
> environment, no tool list, no permission mode, no terminal geometry, no lease, no retention policy
> and no system prompt. Every one of those is a pmux-wide default owned by daemon configuration,
> because a name a caller can write is a name two callers can write — which is exactly how
> `environment.set["CLAUDE_CONFIG_DIR"] = <a live cell's root>` was once admitted into a live minified
> cell. **A caller who cannot name a resource cannot alias one.**

An `agent_id` is a name a caller can write. Two callers can write the same one. Beyond that general
argument, three specific things break:

1. **The pool's class key.** It is `(model, effort)` (`InstanceClass`, `crates/service/src/pool/class.rs:199-202`).
   An agent reference makes it `(model, effort, agent_version)`, so every distinct agent partitions
   the pool. `--path-b-warm MODEL[/EFFORT]=COUNT` (`bin/pmuxd/src/main.rs:120-127`) could no longer
   name a class, and `resolve_pool_class` would need a dimension no operator can pre-declare at boot.
2. **The system prompt.** Path B's is operator-owned, bounded at 512 bytes, and delivered in REPLACE
   mode so it survives `/clear` (`crates/service/src/pool/config.rs:31-40`). An agent carries a
   `system_prompt`. A caller-supplied system prompt is the field `RunStatelessRequest` refuses **by
   name** rather than ignoring, and `deny_unknown_fields` is what performs the refusal
   (`crates/protocol/src/v1.rs:2549-2551`).
3. **Isolation.** The minified cell's whole claim is that the daemon minted both the config root and
   the cwd. An agent that says anything about either is a caller saying something about both.

`RunStatelessRequest` keeps `deny_unknown_fields`, so `{"agent_id": ...}` is refused by name with
`InvalidConfig`. `pmux ask` is unchanged: `(model, effort, prompt) -> text + usage`, and nothing else.

### 6.2 No per-session overrides (`agent_with_overrides`)

CMA has it and ships a field it accepts and ignores. A merge surface needs one documented rule per
field and one test per rule, and nothing derives that list. If you want a variant, `update_agent`
mints a version or `create_agent` mints an agent — both cheap, both named, both pinnable.

### 6.3 No `delete`, no `archive` on the wire

The daemon and its clients run as the same uid (`docs/spec.md:676-678`), so a delete method adds zero
enforcement over `rm -rf <store>/<agent-id>` — which is §4.8's own Argument A, and it applies to this
design's own surface. A running session is unaffected either way, because it pinned by value (§1.4).
CMA needs archive semantics because Anthropic hosts the store; pmux does not host anything.

### 6.4 No server-side `extends`

Inheritance means a stored agent's effective configuration depends on another stored object's
*current* state, which reintroduces the exact impurity versioning was introduced to remove — unless
the parent is also pinned, at which point flattening at create time is the same thing without the
machinery. **Flatten at create.** The stored version is fully resolved. Composition stays a
client-side authoring concern (§4.3).

### 6.5 No discovery, no vault, no `messages[]`

* **No discovery.** No XDG search, no upward walk, no `PMUX_AGENT` defaulting to a name. The rule
  `docs/spec.md:707-711` states — "pmux never *selects* an agent for the caller" — survives verbatim.
* **No vault analogue.** CMA has vaults because Anthropic hosts the sandbox and must inject
  credentials at egress. pmux's child runs as the same uid as the caller and already inherits its
  environment under a published policy. A vault would buy one more place for secrets to sit at rest.
* **No `messages[]`, ever.** §0.1.

---

## 7. Verification plan

Every row: the invariant, the assertion that proves it, and the mutation that must redden it. A row
whose mutation does not redden is a row that is not tested — that is the discipline this repo already
applies (`docs/current-state.md` §9.12, "PROVEN blind, not inferred").

| # | Invariant | Assertion | Mutation that must redden |
|---|---|---|---|
| 1 | A stored version is immutable | `update_agent` twice, then read v1 and assert its bytes are byte-identical to the create response's | Writer truncates `<version>.json` in place instead of writing `<version+1>.json` |
| 2 | A running session is unaffected by an update | Start on v1, `update_agent` to v2, `inspect_session`; assert the snapshot still reports v1's `config_digest` | Session re-reads `head` at turn time instead of holding the pinned value |
| 3 | Resolution is pure | For a table of specs, assert `resolve(spec, per_session) == the inline StartSessionRequest`, compared as `serde_json::Value` | Resolution consults wall-clock, the store, or any daemon state beyond the two inputs |
| 4 | Both-modes is refused, and the field set is **derived** | Two tests, opposite directions: (a) for every leaf path in a fully-populated `AgentSpec`, a request carrying `agent` plus that path is refused; (b) every refused path is a leaf of `AgentSpec` | Replace the derivation with a hand-written array missing one entry — (a) must redden |
| 5 | `environment.snapshot` survives alongside an agent | A request with `agent` + `environment.snapshot` is **accepted** | Intersect at the `environment` key instead of at leaves |
| 6 | Containment narrows only | For a cwd that `admit_bound_resources` already refuses, assert it stays refused under **every** `workspace_root`, including one that contains it | Run the containment check *instead of* `admit_bound_resources` rather than in addition |
| 7 | Containment uses the resolving predicate | A cwd reached via a symlink into `workspace_root` is admitted; one that only *textually* prefixes is not | Replace `one_directory_contains_the_other` with `Path::starts_with` |
| 8 | Store is private at every level pmux creates | Walk the report from `create_private_dir_all` and assert `mode & 0o777 == 0o700` for each; assert every file is `0o600` | `create_dir_all` + one `chmod` on the leaf (the exact defect `private_dir.rs` exists for) |
| 9 | A widened store is refused, never re-permissioned | `chmod 0755` the store, then assert boot refuses **and** the mode is still `0755` | Guard calls `seal_owner_only` on a tree it did not create |
| 10 | A widened agent *file* is refused at read time | `chmod 0644` one version file, then `start_session` referencing it | Mode check runs only at boot |
| 11 | Traversal is unconstructible | Assert the path component is always a canonical hyphenated UUID; assert `name` never reaches a path | Use `spec.name` as the directory name — with `validate_agent_name` admitting `..` (§2.4) this becomes a traversal |
| 12 | The update fence is required and never soft | A stale `expected_version` answers `IdConflict`; assert **no** input answers "already applied" | Add an arm that returns success when the stored digest equals the submitted spec's |
| 13 | No new error code | The existing exhaustive `error_code_name` match (`v1_conformance_vectors.rs:77`) and manifest comparison | Add a variant — the manifest assertion reddens with no other change |
| 14 | Every method AND every event has a golden pair | The corpus's method and event names compared to `manifest.methods` / `manifest.events` **by name**, in all three languages | Append a name to `manifest.methods` or `manifest.events` — MEASURED, the event half was green in all three languages until this row grew its second word |
| 15 | Path B refuses an agent by name | `run_stateless` with `{"agent_id": ...}` answers `InvalidConfig` naming the field | Drop `deny_unknown_fields` from `RunStatelessRequest` |
| 16 | `require_config_isolation: false` + `cell: minified` is refused, not ignored | Assert `create_agent` refuses that spec | Let the minified cell's own requirement override it silently |
| 17 | Redaction holds | No `get_agent`/`list_agents` frame contains an environment value or inline document body; `config_digest` still distinguishes two specs differing only in a redacted value | Digest computed over the redacted spec — the second half reddens |
| 18 | Help and schemas stay honest | The four existing derived tests (`cli.rs:1479`, `:1507`, `:1555`, `:1647`; `tools.rs:1162`) | Ship a subcommand with no `about`, or a schema enum missing a variant |
| 19 | Version publication is atomic AND exclusive | 25 rounds of two concurrent `update_agent` calls on one fence: exactly one winner, an `IdConflict` loser naming the version that now exists, a readable head, a readable pinned v1, an empty `unreadable`, and the winner's digest equal to the store's | `link(2)` replaced by `rename(2)`; or the temporary name made a pure function of the destination again — **both redden** |
| 20 | A listing loses no record it could read | Four states the store can be in (widened `head`, torn version, `head` naming a version never minted, no `head` at all): the readable record survives, the bad one is reported by id, and `reason` is the sentence `get_agent` gives | `list` propagates `?` per entry again |
| 21 | `create` publishes whole or not at all | A reader walking the store while 200 creates run never observes a UUID-named directory that is not already complete | The agent is assembled under its published UUID name instead of a staging one |
| 22 | The per-read guard reads the bytes | A `1.json` that is a symlink to a `0666` file outside the store is refused **whatever the link's own mode is**; so is a directory at that name | `O_NOFOLLOW` dropped, or the `is_file()` check dropped — **both redden** |
| 23 | The serializer refuses all nine derived paths | For every path in `agent_supplied_start_paths()`, a typed request carrying it beside an `agent` fails to serialize naming the path; the same request without an agent serializes | Any subset of the nine reported as absent by the typed presence table |
| 24 | `run_once` decides retention after resolution | `resolve_agent_and_retention` against a real store: `AsResolved` takes the agent's `Persistent`, `ForcedOneShot` yields `OneShot` and a `None` idle TTL, and everything else still comes from the agent | The decision applied before resolution; or `run_once` passing `AsResolved` — **both redden** |
| 25 | The MCP surface delivers the sentence it promises | Every derived path, driven through `map_tool_call`, answers `invalid_config` naming the path with no serde position suffix; a refusal serde wrote stays content-free `invalid_arguments`; the schema's forbidden set equals the derived list | `caller_actionable_decode_refusal` discarded; or its result replaced by the whole rendered error — **both redden** |
| 26 | `--agent` names what the caller did | A typed launch flag is refused by its spelling; the same value read from `env` is overridden and reported by the VARIABLE's name, read from clap's own argument metadata | Value sources not recorded; or the note suppressed; or the variable name replaced by the flag — **all three redden** |
| 27 | A crash between publishing a version and moving `head` leaves the agent USABLE | Rewind `head` to N with N+1 published: `get_agent`/`list_agents` answer N+1, N is still pinnable, two consecutive updates on the stale fence are answered **identically**, and the fence the refusal recommends is accepted | `get`, `summarize` or `update` reads the raw pointer instead of resolving forward — **all three redden**; the forward walk made a one-step lookahead reddens the regressed-pointer test |
| 28 | A taken version NAME that is not a readable version is reported, not stepped over | Three ways to take it (torn file, symlink, directory): `list_agents` reports the record in `unreadable` rather than summarizing it at the older version, `update_agent` refuses naming the file, and the bytes at that name are unchanged | The step predicate narrowed to "a readable version file" — the wedge returns by a second road |

Rows 8, 9, 11, 14, and 17 are the ones I would insist on before anything else: they are the rows where
a green suite over a broken invariant is most plausible.

**Pre-merge, per the house rules:** for every check added, delete it, run its target, confirm the
failure, restore, and verify byte-exact. Clean up daemons and temp roots, then run the residue audit
*after* the E2E runs.

---

## 8. What I would refuse to build without a decision

1. **`docs/spec.md` §4.8 says "pmux has no server-side agent registry and MUST NOT grow one."** This
   design contradicts it. §0.3 argues the amendment — Argument A is conceded, Argument B is answered
   by mandatory version pinning — but §4.4's purity claim also has to be reworded from "a pure
   function of the request" to "a pure function of the request and of the immutable version the
   request names." I will not ship a registry against a live MUST NOT, and I will not quietly edit
   the invariant it contradicts.

2. **The `--agent` CLI collision (§5.2).** Renaming the client-side profile flags to
   `--profile`/`--profile-file` and moving `PMUX_AGENT`/`PMUX_AGENT_FILE` to
   `PMUX_PROFILE`/`PMUX_PROFILE_FILE` is a user-visible break. The alternative — server agents under
   `--agent-id` — leaves two things called "agent" forever. I recommend the rename with a refusal that
   names the new spelling, but it is the owner's call.

3. **The golden-corpus precondition (§3.8).** `golden.json` covers 11 of 12 methods and three
   hand-written `11`s are why. Deriving the count and adding the `run_stateless` pair should land
   *before* any agent method, in its own commit with a per-language mutation proof. If the owner wants
   the agent work first, I need that stated, because the design will otherwise ship a corpus covering
   11 of 16 with a green suite.

4. **Whether `AgentSpec` may carry `environment.set` at all.** I have specified yes, with the store
   owner-only from birth and values redacted on read. The conservative alternative is to refuse `set`
   on an agent entirely and keep every environment value per-invocation, which costs callers the
   repetition an agent exists to remove. This is a security-posture call, not an engineering one.

---

## 9. What was built, and where it deviates from the above

Built across `d310481` (protocol, service, MCP, CLI, docs) on the precondition `ea47ae9`, with the
transport fix in `880e1d6`.

### 9.1 The four §8 decisions, as taken

1. **`docs/spec.md` §4.8 is AMENDED, not deleted.** The retired sentence is quoted in the section
   that replaces it, the uid argument is conceded in full and written as "an agent is not a security
   boundary and MUST NOT be documented as one", and §4.4 now reads "a pure function of the request
   and of the immutable version the request names" with the four properties that make it true.
2. **The `--agent` collision is resolved by renaming the profile flags**, with every retired spelling
   refused and the new one named. `PMUX_AGENT` and `PMUX_AGENT_FILE` are read from the process
   environment expressly so an operator who exported them once is refused rather than silently
   ignored.
3. **The corpus precondition landed first**, in its own commit, with a per-language mutation proof.
4. **`AgentSpec` may carry `environment.set`**, with the store owner-only from birth, values returned
   as digests, and every `CONFIG_ROOT_ENV_DOORS` name refused for every cell.

### 9.2 Deviations from this document, and why

* **Containment uses `directory_lies_within`, not `one_directory_contains_the_other`.** §1.2 said
  route through the latter. It is SYMMETRIC, so with `workspace_root` at `/Users/x/proj` it admits a
  cwd of `/Users/x` -- and `workspace_root`'s own text promises "every session's cwd must resolve
  INSIDE". The new function is the same resolving walk asked in one direction; §1.2's actual concern,
  that a fresh `starts_with` would be wrong under symlinks and the `/tmp` rewrite, is unaffected.
* **`AgentDescriptor::spec` is opaque on the wire**, not a typed `AgentSpec`. §3.3 typed it. A
  request DTO must refuse an unknown field and a response DTO must tolerate one; a single strict type
  on a response would force all three client languages to keep two decoders for one type, and the
  shared corpus asserts additive tolerance at every result object boundary. `AgentDescriptor::spec`
  is the echoed document and `typed_spec()` is where strictness is available.
* **The both-modes refusal is presence-exact via hand-written `Serialize`/`Deserialize`**, not via a
  serializer-collected path walk at admission. §3.6's derivation is real and is what the TEST
  computes; the runtime check needs PRESENCE, which the typed DTO cannot express for five defaulted
  non-`Option` fields, so both impls are written out and the derived list is asserted against them.
* **`pmux agent create` offers `--from-profile`, not the design's bare `--profile`.** Authoring from
  a profile is implemented, and the two profile keys an agent may not carry are refused by name.

### 9.3 One thing this document asserted that turned out to be false

§7 row 17 said a digest computed over the redacted spec would fail to distinguish two agents
differing only in a hidden value. MEASURED: it does not fail -- two different values digest to two
different digests. `docs/current-state.md` §9.16 records the correction and the check that replaced
it.
