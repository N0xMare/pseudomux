use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rmux_sdk::{
    CleanupPolicy, OwnedSession, Pane, PaneProcessState, PaneRef, Rmux, SessionName, Window,
    WindowRef,
};
use thiserror::Error;
use uuid::Uuid;

use crate::{LaunchToken, OwnedProcessBoundary, PROCESS_OBSERVATION_POLL_INTERVAL};

const CONTROL_PLANE_CLEANUP_GRACE: Duration = Duration::from_secs(1);

/// Window/pane slot every private pmux terminal is launched into.
///
/// Kept as a named constant, addressed only through [`private_pane_ref`] and
/// [`private_window_ref`], so the launch handle in [`TerminalBackend::create`]
/// and every throwaway handle minted by [`RmuxTerminal::operation_pane`],
/// [`RmuxTerminal::write_pane`] and [`RmuxTerminal::write_window`] address
/// exactly the same slot. These must never drift apart — and after layer (b)
/// there are four minting sites rather than two, so the constant is doing more
/// work than it was.
const PRIVATE_PANE_SLOT: (u32, u32) = (0, 0);

/// Configuration for an explicit, already-running private rmux daemon.
#[derive(Clone, Debug)]
pub struct RmuxBackendConfig {
    pub socket: PathBuf,
    pub launcher: PathBuf,
    pub launcher_socket: PathBuf,
    pub operation_timeout: Duration,
    pub lease_ttl: Duration,
}

/// Values required to create one leased foreground terminal.
#[derive(Clone, Debug)]
pub struct TerminalLaunch {
    pub session_id: Uuid,
    pub cwd: PathBuf,
    pub rows: u16,
    pub cols: u16,
    pub launch_token: LaunchToken,
}

/// Stable diagnostic reference. It is never the public pmux session ID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendSessionRef {
    pub rmux_session_name: String,
    pub pane_id: u32,
}

/// Rendered terminal snapshot used only for readiness, interaction, and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalSnapshot {
    pub revision: u64,
    pub rows: u16,
    pub cols: u16,
    /// Real rmux snapshots always carry a cursor. `None` is retained only so
    /// cursor-less test doubles can exercise the legacy text classifier.
    pub cursor: Option<TerminalCursor>,
    pub visible_text: String,
}

/// Structured cursor state copied from rmux without exposing the terminal
/// cell grid to higher layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TerminalCursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    pub style: u32,
}

/// The foreground colour of one captured cell, reduced to the two questions a
/// screen proof can honestly ask of it.
///
/// It is deliberately not a colour: pmux must never decide anything from what a
/// palette index *looks* like. The only supported operations are equality
/// between two cells and "was a colour explicitly selected here at all", which
/// is exactly what identifying a same-styled run of cells requires and nothing
/// more.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CellColor {
    /// The terminal default, the explicit no-colour sentinel, or the terminal
    /// sentinel. No colour was selected for this cell.
    Unstyled,
    /// A colour was selected. The payload is rmux's own raw encoding, carried
    /// as an opaque identity token; it is compared, never interpreted.
    Explicit(i32),
}

impl CellColor {
    #[must_use]
    pub const fn is_styled(self) -> bool {
        matches!(self, Self::Explicit(_))
    }

    /// One 256-colour palette entry in rmux's own raw encoding.
    ///
    /// The single place that knows how a palette index becomes an identity
    /// token, so a fixture rebuilt from a capture and a live capture of the
    /// same screen produce the same value. Production never calls this: it only
    /// ever compares whole cells to each other.
    #[must_use]
    pub fn indexed(index: u8) -> Self {
        Self::from(rmux_sdk::PaneColor::indexed(index))
    }
}

impl From<rmux_sdk::PaneColor> for CellColor {
    fn from(value: rmux_sdk::PaneColor) -> Self {
        match value {
            rmux_sdk::PaneColor::Default
            | rmux_sdk::PaneColor::None
            | rmux_sdk::PaneColor::Terminal => Self::Unstyled,
            styled => Self::Explicit(styled.encoded()),
        }
    }
}

/// One captured cell: its glyph payload and its foreground colour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyledCell {
    pub text: String,
    pub foreground: CellColor,
    padding: bool,
}

impl StyledCell {
    /// A rendered, non-padding cell.
    #[must_use]
    pub fn new(text: impl Into<String>, foreground: CellColor) -> Self {
        Self {
            text: text.into(),
            foreground,
            padding: false,
        }
    }

    /// The trailing half of a double-width glyph.
    ///
    /// Exists so a recorded screen can be reconstructed byte-exactly. Padding is
    /// the one cell property that changes `row_text` without changing any cell's
    /// glyph, so a corpus that could not rebuild it would replay a *different*
    /// screen than the one production classified — silently, and only for rows
    /// containing wide characters.
    #[must_use]
    pub fn padding(text: impl Into<String>, foreground: CellColor) -> Self {
        Self {
            text: text.into(),
            foreground,
            padding: true,
        }
    }

    /// Whether this cell is the trailing half of a wide glyph. Padding cells
    /// carry no glyph of their own and are skipped when a row is rendered.
    #[must_use]
    pub const fn is_padding(&self) -> bool {
        self.padding
    }
}

/// The same captured frame as [`TerminalSnapshot`], with the per-cell
/// foreground colours that [`TerminalSnapshot::visible_text`] throws away.
///
/// This exists for exactly one caller: the proof that the slash-command entry
/// Enter is about to select is the command pmux typed. MEASURED on Claude Code
/// 2.1.220, that menu marks its selected row with a foreground colour change
/// and nothing else — no reverse video, no background, no attribute bit and no
/// glyph marker — so the selected row and an unselected one are byte-identical
/// in `visible_text`. A proof built on plain text cannot see the selection at
/// all; it is not merely hard to assert, it is absent from the data.
///
/// It is a separate read rather than a widening of [`TerminalSnapshot`] on
/// purpose. `TerminalSnapshot` equality is the fence every input gate settles
/// on, and folding cell attributes into that comparison would silently change
/// what "the screen held still" means for every gate in the service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyledScreen {
    pub revision: u64,
    pub rows: u16,
    pub cols: u16,
    pub cursor: Option<TerminalCursor>,
    /// Row-major. `rows` entries; a malformed capture may render a short row,
    /// which is preserved rather than padded.
    cells: Vec<Vec<StyledCell>>,
}

impl StyledScreen {
    /// Builds a screen from already-captured rows.
    #[must_use]
    pub fn new(
        revision: u64,
        rows: u16,
        cols: u16,
        cursor: Option<TerminalCursor>,
        cells: Vec<Vec<StyledCell>>,
    ) -> Self {
        Self {
            revision,
            rows,
            cols,
            cursor,
            cells,
        }
    }

    /// The captured cells of one row, or an empty slice past the frame.
    #[must_use]
    pub fn row(&self, row: u16) -> &[StyledCell] {
        self.cells
            .get(usize::from(row))
            .map_or(&[], |cells| cells.as_slice())
    }

    /// One row rendered exactly as rmux's lossy plain text renders it: padding
    /// cells skipped, trailing spaces trimmed, everything else verbatim.
    ///
    /// This must stay byte-identical to `PaneSnapshot::row_text`, because the
    /// selection proof reads text from here and every other gate reads it from
    /// [`TerminalSnapshot::visible_text`]. `styled_text_matches_lossy_pane_text`
    /// is the test that holds the two together.
    #[must_use]
    pub fn row_text(&self, row: u16) -> String {
        render_cells_lossy(self.row(row))
    }

    /// Every row joined by `\n`, with no synthetic trailing newline.
    #[must_use]
    pub fn visible_text(&self) -> String {
        (0..self.rows)
            .map(|row| self.row_text(row))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The plain-text view of this same frame, for the gates that compare
    /// against a [`TerminalSnapshot`] fence.
    #[must_use]
    pub fn to_terminal_snapshot(&self) -> TerminalSnapshot {
        TerminalSnapshot {
            revision: self.revision,
            rows: self.rows,
            cols: self.cols,
            cursor: self.cursor,
            visible_text: self.visible_text(),
        }
    }
}

fn render_cells_lossy(cells: &[StyledCell]) -> String {
    let mut rendered = String::new();
    for cell in cells {
        if cell.is_padding() {
            continue;
        }
        rendered.push_str(&cell.text);
    }
    while rendered.ends_with(' ') {
        rendered.pop();
    }
    rendered
}

#[derive(Debug, Error)]
pub enum TerminalBackendError {
    #[error("invalid terminal launch: {0}")]
    InvalidLaunch(String),
    /// A private rmux control-plane connection was lost.
    ///
    /// Before per-session transports this meant the whole private daemon:
    /// every terminal shared one connection, and the SDK's write-once poison
    /// latch made a single lost request permanently fatal for all of them. Each
    /// terminal now owns its connections, so when this comes from a terminal it
    /// is scoped to the terminal that produced it and says nothing on its own
    /// about the sidecar or about any sibling session.
    /// [`TerminalSession::lease_lost`] is the discriminator for the stronger
    /// claim; see the `TerminalBackendError` arm of
    /// `pseudomux_service::driver_io::map_terminal_error`.
    ///
    /// The `Display` deliberately does **not** say "session". This is also the
    /// variant [`RmuxBackend::probe_control_plane`] returns, and that call runs
    /// at `PrivateRuntime::start` before any session exists — there the fault
    /// genuinely is daemon-wide, and `pseudomux_service::runtime` renders it
    /// inside `private rmux control plane is unusable: {this}`. A message
    /// asserting a session there would have been false. Scope is stated by
    /// whoever has the evidence for it: the terminal mappers say "session", the
    /// startup probe does not.
    #[error("a private rmux control-plane connection was lost")]
    ControlPlaneLost,
    #[error("rmux operation failed: {0}")]
    Rmux(String),
    #[error("terminal process-boundary verification failed: {0}")]
    ProcessBoundary(String),
}

impl From<rmux_sdk::RmuxError> for TerminalBackendError {
    fn from(value: rmux_sdk::RmuxError) -> Self {
        // WaitTimeoutError's Display includes both the matcher and the last
        // visible screen. Neither is safe at this boundary: a matcher can be
        // caller-controlled and the screen can contain prompts, paths, or
        // account data.
        match value {
            rmux_sdk::RmuxError::Transport { .. }
            | rmux_sdk::RmuxError::OwnedSessionLeaseLost { .. } => Self::ControlPlaneLost,
            rmux_sdk::RmuxError::WaitTimeout { source, .. } => Self::Rmux(format!(
                "terminal wait timed out after {} ms",
                source.timeout().as_millis()
            )),
            other => Self::Rmux(other.to_string()),
        }
    }
}

#[async_trait]
pub trait TerminalBackend: Send + Sync {
    async fn create(
        &self,
        launch: TerminalLaunch,
    ) -> Result<Box<dyn TerminalSession>, TerminalBackendError>;
}

#[async_trait]
pub trait TerminalSession: Send {
    fn backend_ref(&self) -> &BackendSessionRef;
    fn lease_lost(&self) -> bool;
    async fn snapshot(&self) -> Result<TerminalSnapshot, TerminalBackendError>;
    /// One captured frame with its per-cell foreground colours retained.
    ///
    /// Deliberately not a defaulted method. A default would let a session that
    /// cannot see cell colours silently answer "no colours here", and the one
    /// caller — the pre-Enter selection proof — would then refuse or, worse, be
    /// tempted to treat absent evidence as benign. Every implementation states
    /// what it can produce.
    async fn styled_screen(&self) -> Result<StyledScreen, TerminalBackendError>;
    async fn wait_visible_text(
        &self,
        needle: &str,
        timeout: Duration,
    ) -> Result<TerminalSnapshot, TerminalBackendError>;
    async fn wait_quiet(
        &self,
        stable_for: Duration,
        timeout: Duration,
    ) -> Result<TerminalSnapshot, TerminalBackendError>;
    async fn paste(&mut self, text: &str) -> Result<(), TerminalBackendError>;
    async fn enter(&mut self) -> Result<(), TerminalBackendError>;
    async fn interrupt(&mut self) -> Result<(), TerminalBackendError>;
    async fn resize(&mut self, rows: u16, cols: u16) -> Result<(), TerminalBackendError>;
    /// Requests teardown and returns `true` only after the isolated process
    /// boundary has been positively observed empty.
    async fn close(&mut self) -> Result<bool, TerminalBackendError>;
}

/// Real rmux implementation. Construction never starts, discovers, or contacts
/// a daemon.
pub struct RmuxBackend {
    /// Inert facade, kept for exactly one purpose: [`Self::probe_control_plane`].
    ///
    /// It is deliberately *not* the transport any session runs on. This backend
    /// used to hold a `connect()`ed facade whose single `TransportClient` was
    /// cloned into every `OwnedSession`, every `Session`, and every `Pane` in
    /// the process, so one poisoned request took every session with it (the
    /// poison latch is per connection: `TransportState` is allocated fresh per
    /// `TransportClient::spawn`, rmux-sdk transport/mod.rs:50, and
    /// `set_terminal_failure` is write-once, transport/state.rs:39-44).
    /// [`TerminalBackend::create`] now builds one lazy facade per session and
    /// this one holds no transport at all, so nothing minted from it can be
    /// shared by two terminals.
    probe: Rmux,
    config: RmuxBackendConfig,
}

impl RmuxBackend {
    /// Records configuration and builds the inert probe facade.
    ///
    /// Deliberately not named `connect` and deliberately not `async`: after the
    /// move to per-session transports there is nothing left here to connect.
    /// `RmuxBuilder::build` records configuration and never contacts the daemon
    /// (rmux-sdk handles/builder.rs:105-107), so the only failure this can
    /// report is a malformed endpoint set. Reachability is
    /// [`Self::probe_control_plane`]'s job, and a caller that wants a startup
    /// reachability check must call it — see `PrivateRuntime::start`.
    pub fn configure(config: RmuxBackendConfig) -> Result<Self, TerminalBackendError> {
        if !config.socket.is_absolute()
            || !config.launcher.is_absolute()
            || !config.launcher_socket.is_absolute()
        {
            return Err(TerminalBackendError::InvalidLaunch(
                "rmux socket, launcher, and launcher socket must be absolute".into(),
            ));
        }
        let probe = Rmux::builder()
            .unix_socket(config.socket.clone())
            .default_timeout(config.operation_timeout)
            .build();
        Ok(Self { probe, config })
    }

    /// Proves the private control plane is reachable, on a connection nothing
    /// else will ever use.
    ///
    /// This exists because [`Self::configure`] removed the `.connect()` that
    /// used to sit in this backend's constructor, and that `.connect()` was
    /// carrying a second, unnamed job: it was the startup reachability check.
    /// Without a replacement, an unreachable socket would first be observed at
    /// the first `start_session` — long after the daemon had reported itself
    /// healthy — and would be reported as a session failure rather than as a
    /// failed start.
    ///
    /// `Rmux::capabilities` on an inert facade opens one `UnixStream`, issues
    /// one `Handshake`, and drops the connection when the local client goes out
    /// of scope (rmux-sdk handles/rmux.rs:268-276, capabilities.rs:47-49). That
    /// is strictly more evidence than `.connect()` gave, which only opened the
    /// socket: a path that accepts connections but does not speak the rmux wire
    /// protocol passed the old check and fails this one. No capability is
    /// *required* here on purpose; the exact capability set each session needs
    /// is preflighted per session by `owned_session`
    /// (rmux-sdk handles/owned_session.rs:104-120).
    ///
    /// ## What it does not prove
    ///
    /// It proves the *protocol*, not dispatch health. `Request::Handshake` is
    /// answered at the top of `dispatch_request`, before anything acquires the
    /// sidecar's global handler state lock
    /// (vendor/rmux-server/src/handler_dispatch.rs:232-246 returns the
    /// handshake response directly, before any lock; the lock is
    /// `state: Arc<tokio::Mutex<HandlerState>>`, handler.rs:217). A sidecar
    /// whose dispatch lock is held by a wedged handler therefore *passes* this
    /// probe while being unable to serve a single session request. Startup
    /// reachability is all this is for; liveness of the request path is the
    /// doctor probe's job, and it needs a request that actually takes that lock
    /// — `ListSessions` does, at handler_session.rs:1606 — rather than a
    /// handshake.
    pub async fn probe_control_plane(&self) -> Result<(), TerminalBackendError> {
        self.probe.capabilities().await?;
        Ok(())
    }

    /// Completes one real request on the sidecar's *dispatch* path and returns
    /// the exact private terminals the sidecar itself says it is hosting.
    ///
    /// This is the cheapest operation that touches everything a health report
    /// wants to claim, and the choice was made against measurements rather than
    /// against the shape of the code:
    ///
    /// * It is `list-sessions` and not a handshake because a handshake is
    ///   answered before any lock is taken — see the section above. Only a
    ///   request that takes `state: Arc<tokio::Mutex<HandlerState>>` can observe
    ///   a dispatch path that has stopped serving; `list-sessions` takes it at
    ///   `vendor/rmux-server/src/handler_session.rs:1606`.
    /// * It is `list-sessions` and not a per-session capture because a capture
    ///   of a destroyed session **succeeds**. Measured: after the pane process
    ///   was `SIGKILL`ed, `TerminalSession::snapshot` kept returning `Ok` for
    ///   the full six seconds it was polled, while the session had already
    ///   disappeared from `list-sessions` inside 500 ms. A per-session capture
    ///   probe would have reported every one of those seconds as healthy.
    /// * It is `list-sessions` and not `TerminalSession::lease_lost` because the
    ///   lease is a heartbeat and therefore lags by up to its TTL. Measured in
    ///   the same run: `lease_lost()` stayed `false` for ~2 s (the configured
    ///   TTL) after the process was killed, and it is a local flag read that
    ///   completes happily while the sidecar is unreachable.
    /// * It is one request for the whole daemon, not one per session. A pool
    ///   with fifteen warm sidecar sessions costs exactly one round trip, so
    ///   the probe's price does not grow with the thing it is protecting.
    ///
    /// Returns the raw session names. The caller owns the reconciliation,
    /// because only the caller knows which names it expects; this layer knows
    /// only what the daemon said.
    pub async fn probe_request_path(&self) -> Result<Vec<String>, ControlPlaneFault> {
        match self.probe.list_sessions().await {
            Ok(names) => Ok(names
                .into_iter()
                .map(|name| name.as_str().to_owned())
                .collect()),
            Err(error) => Err(ControlPlaneFault::classify(&error)),
        }
    }
}

/// Why one control-plane probe did not complete.
///
/// Deliberately narrower than [`TerminalBackendError`], which flattens every
/// transport failure into the single [`TerminalBackendError::ControlPlaneLost`]
/// variant. That flattening is right for a session -- a session cannot act
/// differently on "refused" than on "timed out" -- and wrong for a health
/// report, whose entire job is to say which of the two happened. Measured
/// shapes, from a real sidecar:
///
/// * `SIGKILL`ed sidecar:  `Transport { operation: "connect to rmux daemon",
///   source: Os { code: 61, kind: ConnectionRefused } }`, returned in 0 ms.
/// * `SIGSTOP`ped sidecar: `Transport { operation: "complete \`list-sessions\`
///   request/response exchange with rmux daemon", source: Custom { kind:
///   TimedOut } }`, returned at the configured deadline.
/// * Peer that accepts and then dies mid-exchange: `Transport { operation:
///   "complete \`list-sessions\` request/response exchange with rmux daemon",
///   source: Custom { kind: BrokenPipe, error: "Broken pipe (os error 32)" } }`.
///   MEASURED here, in
///   [`a_peer_that_dies_mid_exchange_is_reported_unreachable_like_an_absent_one`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlPlaneFault {
    /// A transport failure that is not a deadline expiry.
    ///
    /// NAMED FOR THE CASE THAT DOMINATES IT, NOT FOR THE SET IT HOLDS. The
    /// measured, and by far the commonest, member is the `ConnectionRefused`
    /// above: no connection to the private socket, nothing dispatched. But this
    /// is the residual arm of `Self::classify` -- private, so deliberately not
    /// an intra-doc link from public documentation -- so every `io::ErrorKind`
    /// other than `TimedOut` lands here -- including the `BrokenPipe` of a peer
    /// that died mid-exchange, MEASURED above, where a connection did exist and
    /// a request WAS dispatched. The old wording, "No usable connection to the
    /// private socket. Nothing was dispatched.", was false for that case.
    ///
    /// That residual arm is deliberate and the doc is what moved. There is no
    /// third answer to give a mid-exchange death: it is not
    /// [`Self::Unresponsive`], which means specifically that the deadline
    /// expired with the connection still open, and it is not [`Self::Refused`],
    /// which means the daemon answered. A fourth variant would have to appear
    /// on the wire in `RuntimeFinding`, in `runtime_finding_text`, and in every
    /// client that renders it, to tell an operator to do the identical thing:
    /// the sidecar is not serving, restart the runtime. So read this as "no
    /// answer, and not because the deadline expired" -- not as "nothing was
    /// dispatched".
    Unreachable,
    /// A connection existed and the request did not complete inside the same
    /// deadline every session operation is held to.
    Unresponsive,
    /// The daemon answered, and the answer was an error.
    Refused,
}

impl ControlPlaneFault {
    /// Classifies by the transport's own `io::ErrorKind`, not by rendered text.
    ///
    /// Deliberately not `Display`-matching: the SDK's transport messages embed
    /// the operation name and the endpoint, both of which change without a
    /// version bump, and a classifier that reads them is a guard whose
    /// predicate is a substring.
    fn classify(error: &rmux_sdk::RmuxError) -> Self {
        match error {
            rmux_sdk::RmuxError::Transport { source, .. } => match source.kind() {
                std::io::ErrorKind::TimedOut => Self::Unresponsive,
                _ => Self::Unreachable,
            },
            _ => Self::Refused,
        }
    }
}

#[async_trait]
impl TerminalBackend for RmuxBackend {
    async fn create(
        &self,
        launch: TerminalLaunch,
    ) -> Result<Box<dyn TerminalSession>, TerminalBackendError> {
        if launch.rows == 0 || launch.cols == 0 || !launch.cwd.is_absolute() {
            return Err(TerminalBackendError::InvalidLaunch(
                "terminal dimensions must be non-zero and cwd must be absolute".into(),
            ));
        }

        // The public Claude session ID is deterministic, while this private
        // generation suffix prevents an unconsumed attach capability from a
        // prior generation resolving a newly resumed session with the same ID.
        let name = private_session_name(launch.session_id)?;

        // One lazy facade per session. `RmuxBuilder::build` records
        // configuration and holds no transport, so every handle minted from it
        // opens its own `UnixStream`
        // (rmux-sdk handles/rmux.rs:433-453). This is the whole of layer (c):
        // the poison latch is per connection, so a session that owns its
        // connections cannot latch a sibling's.
        let session_rmux = Arc::new(
            Rmux::builder()
                .unix_socket(self.config.socket.clone())
                .default_timeout(self.config.operation_timeout)
                .build(),
        );

        let mut owned = session_rmux
            .owned_session(name.clone())
            .cleanup_policy(CleanupPolicy::KillOnOwnerExit)
            .lease_ttl(self.config.lease_ttl)
            .await?;

        // Deliberately `Rmux::pane` and not `OwnedSession::pane`. The latter
        // clones the owned session's own transport (rmux-sdk
        // handles/session.rs:152-159), which is the connection `owned.cleanup()`
        // runs on; a write that poisoned it would take teardown with it, and
        // teardown is the last thing standing between an abandoned session and a
        // surviving interactive process. `Rmux::pane` on a lazy facade opens a
        // fresh connection instead.
        //
        // THIS HANDLE IS LOCAL TO `create` AND IS NOT RETAINED. It launches the
        // pane, reads its id, and observes the process boundary, and then it is
        // dropped with its connection. It used to be kept as the terminal's
        // write handle for life, which is precisely the thing layer (b) closed:
        // a `Pane` captures its `TransportClient` at construction and
        // `TransportState::set_terminal_failure` is write-once (rmux-sdk
        // transport/state.rs:39-44), so one aborted write left every later
        // write on the terminal failing forever. Writes now mint their own
        // connection through [`RmuxTerminal::write_pane`], exactly as reads do.
        // A terminal therefore owns TWO long-lived connections -- the owned
        // session and its lease heartbeat (rmux-sdk
        // owned_session/lease.rs:39, :214-217) -- and every read and every write
        // mints a transient third.
        //
        // It is also deliberately the *slot* form and not `pane_by_id`. On a
        // miss -- which is to say from the moment the pane dies --
        // `current_pane_ref_for_id` fans out `list-sessions` plus one
        // `list-panes` per session, twice, per call. That is O(N) RPCs on a
        // 20 ms poll of a dead pane, and it can push a single poll past the
        // SDK's operation deadline, poisoning the very connection per-session
        // transports exist to protect.
        let pane = match session_rmux.pane(private_pane_ref(&name)).await {
            Ok(pane) => pane,
            Err(error) => {
                let _ = owned.cleanup().await;
                return Err(error.into());
            }
        };

        // THE WINDOW IS THE RESOURCE; THE PANE ONLY INHERITS FROM IT.
        //
        // This used to be `pane.resize(TerminalSizeSpec::new(cols, rows))`, and
        // it silently delivered 24x80 for every geometry ever requested --
        // including `bin/pmux/src/cli.rs`'s `DEFAULT_COLS: u16 = 120`. The
        // request was accepted and discarded three layers down:
        //
        // * The SDK turns `Pane::resize` into `resize-pane -x <cols>` /
        //   `-y <rows>` (rmux-sdk pane/input.rs:42-70).
        // * `OwnedSessionBuilder` creates the session with no size and exposes
        //   no way to set one, so rmux uses `DEFAULT_SESSION_SIZE = 80x24`
        //   (vendor/rmux-server/src/handler.rs:188).
        // * For a single-pane window `Window::resize_pane_width` records a
        //   `requested_main_width` and then rebuilds the layout tree against
        //   the WINDOW's size (rmux-core window/layout_ops.rs:96-115, :285-295).
        //   `requested_main_width` only governs the main pane of a
        //   main-vertical layout with siblings; a lone pane fills its window and
        //   cannot exceed it. 120 collapsed back to 80 and `resize-pane`
        //   returned success.
        //
        // Resizing the WINDOW is what the pane's size is actually derived from:
        // `resize_window` sets the window size directly and resizes the backing
        // terminals with it (vendor/rmux-server/src/pane_terminals_window.rs:375-436).
        // Both dimensions go in one request, so there is no window in which the
        // pane is half-resized.
        //
        // A FRESH CONNECTION, for the same reason `Rmux::pane` is used below and
        // not `OwnedSession::pane`: a write that poisoned the owned session's
        // transport would take `owned.cleanup()` with it, and teardown is the
        // last thing standing between an abandoned session and a surviving
        // interactive process.
        let window = match session_rmux.window(private_window_ref(&name)).await {
            Ok(window) => window,
            Err(error) => {
                let _ = owned.cleanup().await;
                return Err(error.into());
            }
        };

        let setup = async {
            window.resize(Some(launch.cols), Some(launch.rows)).await?;
            pane.spawn([
                self.config.launcher.to_string_lossy().into_owned(),
                "--socket".into(),
                self.config.launcher_socket.to_string_lossy().into_owned(),
                "--token".into(),
                launch.launch_token.expose().to_owned(),
            ])
            .cwd(launch.cwd)
            .kill_existing(true)
            .await?;
            pane.id().await
        }
        .await;

        let pane_id = match setup {
            Ok(Some(pane_id)) => pane_id,
            Ok(None) => {
                let _ = owned.cleanup().await;
                return Err(TerminalBackendError::Rmux(
                    "rmux did not return a pane id after launcher spawn".into(),
                ));
            }
            Err(error) => {
                let _ = owned.cleanup().await;
                return Err(error.into());
            }
        };

        let boundary =
            match observe_process_boundary(&pane, pane_id.as_u32(), self.config.operation_timeout)
                .await
            {
                Ok(boundary) => boundary,
                Err(error) => {
                    let _ = owned.cleanup().await;
                    return Err(error);
                }
            };

        // The launch handle and its connection end here. Everything after this
        // point mints its own.
        drop(pane);

        Ok(Box::new(RmuxTerminal {
            owned,
            rmux: session_rmux,
            session_name: name.clone(),
            session_id: launch.session_id,
            reference: BackendSessionRef {
                rmux_session_name: name.to_string(),
                pane_id: pane_id.as_u32(),
            },
            boundary,
            write_order: Arc::new(tokio::sync::Mutex::new(())),
            cleanup_timeout: self.config.operation_timeout,
            cleanup_requested: false,
            escaped_descendant_observed: false,
            closed: false,
        }))
    }
}

/// The one pane every private terminal addresses, in the one form every handle
/// must use. See [`PRIVATE_PANE_SLOT`].
fn private_pane_ref(session_name: &SessionName) -> PaneRef {
    let (window_index, pane_index) = PRIVATE_PANE_SLOT;
    PaneRef::new(session_name.clone(), window_index, pane_index)
}

/// The window the private pane lives in, addressed from the same constant.
///
/// Derived from [`PRIVATE_PANE_SLOT`] rather than written out, so the window
/// that gets resized and the pane that gets written to can never come to
/// disagree about which slot they mean.
fn private_window_ref(session_name: &SessionName) -> WindowRef {
    let (window_index, _) = PRIVATE_PANE_SLOT;
    WindowRef::new(session_name.clone(), window_index)
}

fn private_session_name(session_id: Uuid) -> Result<SessionName, TerminalBackendError> {
    SessionName::new(format!(
        "pmux-{}-{}",
        session_id.simple(),
        Uuid::new_v4().simple()
    ))
    .map_err(|error| TerminalBackendError::InvalidLaunch(error.to_string()))
}

async fn observe_process_boundary(
    pane: &Pane,
    pane_id: u32,
    timeout: Duration,
) -> Result<OwnedProcessBoundary, TerminalBackendError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let snapshot = pane.info().await?;
        let process = snapshot
            .panes
            .iter()
            .find(|info| info.id.as_u32() == pane_id)
            .map(|info| &info.process);
        match process {
            Some(PaneProcessState::Running { pid: Some(pid) }) => {
                if let Some(boundary) = OwnedProcessBoundary::capture(*pid)
                    .map_err(|error| TerminalBackendError::ProcessBoundary(error.to_string()))?
                {
                    return Ok(boundary);
                }
            }
            Some(PaneProcessState::Exited) => {
                return Err(TerminalBackendError::ProcessBoundary(
                    "pane process exited before its isolated session could be observed".into(),
                ));
            }
            Some(PaneProcessState::Running { pid: None } | PaneProcessState::Unknown) | None => {}
            Some(_) => {}
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(TerminalBackendError::ProcessBoundary(format!(
                "rmux did not expose an isolated process identity for pane {pane_id} before the deadline"
            )));
        }
        tokio::time::sleep(PROCESS_OBSERVATION_POLL_INTERVAL).await;
    }
}

/// One leased private terminal, with cancellation safety supplied *beneath*
/// the [`TerminalSession`] trait.
///
/// ## Why every rmux call in here looks unusual
///
/// rmux-sdk treats a dropped in-flight request as a permanent transport
/// failure. `OrderedResponseGuard::drop` aborts the connection when its request
/// never completed (rmux-sdk transport/cancellation.rs:27-34), that calls
/// `TransportClient::abort_with` (transport/mod.rs:268-271), and
/// `TransportState::set_terminal_failure` is write-once and is never cleared
/// (transport/state.rs:39-44). There is no reconnect, reset, or inspect API on
/// `Rmux`. So *any* `tokio::time::timeout(_, pane.something())` whose timer
/// wins is a permanent kill of that connection — and the census found **twelve**
/// such call sites in pmux, plus a **thirteenth** that is not a pmux call site
/// at all.
///
/// Fixing it at the twelve was rejected. Their control flow and error mapping
/// are correct and are covered by tests against in-crate fakes, and five of them
/// are reached only indirectly through the actor's turn loop. The thirteenth
/// settles it: the SDK's own `tokio::time::timeout(remaining, pane.snapshot())`
/// at rmux-sdk wait.rs:333 is inside the SDK, so no change on this side of the
/// API can reach it. The fix therefore lives here, on the leaf, in two shapes:
///
/// * **Writes** (`paste`, `enter`, `interrupt`, `resize`) run on a detached
///   `tokio::spawn`, so dropping the caller's future never drops the request.
///   See [`RmuxTerminal::detached_write`] for why the FIFO `write_order` gate
///   is load-bearing rather than decorative.
/// * **Reads** (`snapshot`) and the SDK's own polling waits
///   (`wait_visible_text`, `wait_quiet`) run on a throwaway handle minted from
///   [`RmuxTerminal::rmux`], so a drop poisons a connection that nothing
///   else will ever use. This is the only shape that covers wait.rs:333,
///   because that timeout is inside the SDK.
///
/// ## Layer (b): every connection this terminal uses is minted per operation
///
/// Writes used to be the exception. They rode one `Pane` captured at `create`
/// for the terminal's whole life, and rmux-sdk binds a handle to its
/// `TransportClient` at construction while `TransportState::set_terminal_failure`
/// is write-once and never cleared (transport/state.rs:39-44). So a single
/// aborted write — the SDK's own `operation_timeout` firing against a stalled
/// sidecar is enough, with nothing abandoned by pmux at all — left `paste`,
/// `enter` and `interrupt` failing on that terminal *forever*, while
/// `map_terminal_error` went on answering `DaemonLost retryable: true`. That
/// was recorded as (b)'s residue and it is now closed the same way reads were:
/// [`RmuxTerminal::write_pane`] and [`RmuxTerminal::write_window`] mint a
/// handle per write from the retained lazy facade, so the connection a write
/// latches is one no later write will use. Regression:
/// `private_runtime.rs::private_terminal_write_recovers_after_the_sdk_aborts_
/// its_write_transport`.
///
/// There is deliberately no epoch, no watch channel and no rebuild budget. The
/// `ControlPlane` (b) was originally specified as was designed for the one
/// process-wide transport layer (c) deleted; with nothing daemon-wide left to
/// rebuild, "recover the control plane" and "mint the next handle" are the same
/// operation, and the second one needs no state.
///
/// Do not reintroduce a retained `Pane` or `Window` on this struct.
///
/// ## What this terminal owns, and why none of it is shared
///
/// Every connection named below belongs to this terminal alone. That is layer
/// (c), and it is what makes the paragraph above a *bounded* claim rather than
/// a daemon-wide one: "a connection nothing else will ever use" is only true
/// once no connection is shared. What remains shared is stated where it can be
/// acted on rather than hidden here — the sidecar funnels dispatch through one
/// global handler-state lock, so a handler stall still stalls every connection
/// at once.
struct RmuxTerminal {
    owned: OwnedSession,
    /// This session's own lazy facade, and the origin of every connection the
    /// terminal owns.
    ///
    /// Built with `build()` and never `connect()`, so it holds no transport and
    /// each handle minted from it opens its own `UnixStream`
    /// (rmux-sdk handles/rmux.rs:433-453). Exactly two long-lived connections
    /// came out of it in [`TerminalBackend::create`] — the owned session and
    /// that session's lease heartbeat — and every read and every write mints a
    /// transient third.
    ///
    /// `Arc` because a detached write outlives the caller that issued it by
    /// construction, so the facade it mints from has to be nameable from the
    /// spawned task. `Rmux` is not `Clone`, and a facade from `build()` carries
    /// `transport: None` and `DropGuard::noop()` (rmux-sdk
    /// handles/rmux.rs:352-359, transport/mod.rs:288-292), so sharing it costs
    /// one refcount and its eventual drop does nothing.
    ///
    /// Minting a handle issues no request of its own -- `Rmux::pane`
    /// connects and returns (handles/rmux.rs:172-184) -- but the first
    /// operation on the fresh connection does pay one extra `Handshake`,
    /// because the negotiated-capability cache lives on the `TransportClient`
    /// and is therefore per connection (rmux-sdk capabilities.rs:51-58,
    /// transport/mod.rs:227-233). No latency figure is quoted anywhere in this
    /// file: none was measured for this change, and an inherited one is not
    /// evidence.
    ///
    /// Retained rather than dropped after `create` because it is the only way
    /// to a working connection once one of this terminal's own has latched:
    /// there is no reconnect, reset, or inspect API on `Rmux`, so recovery is
    /// always "mint a new handle from the facade".
    rmux: Arc<Rmux>,
    /// Retained so every minted handle addresses the same slot.
    session_name: SessionName,
    /// Retained for diagnostics only. Never used to address rmux.
    session_id: Uuid,
    reference: BackendSessionRef,
    boundary: OwnedProcessBoundary,
    /// Per-terminal FIFO gate that orders detached writes against each other
    /// and against teardown. See [`RmuxTerminal::detached_write`].
    write_order: Arc<tokio::sync::Mutex<()>>,
    cleanup_timeout: Duration,
    cleanup_requested: bool,
    escaped_descendant_observed: bool,
    closed: bool,
}

impl RmuxTerminal {
    /// Maps an SDK error and, when it classifies as a control-plane loss,
    /// records exactly one WARN line for it.
    ///
    /// Only the static operation name, the pmux session id, the pane id, and
    /// the session's lease state are logged. The error's `Display` is
    /// deliberately not logged: rmux errors can carry session names, paths,
    /// matchers, and rendered screen text, and this process handles prompts and
    /// account state.
    ///
    /// `lease_lost` is on the line because after layer (c) the loss is scoped
    /// to one session, and an operator reading "control plane lost" needs to
    /// know which of two very different things happened. It is weak evidence in
    /// both directions, and the wording has to say so.
    ///
    /// `true` says the lease heartbeat stopped renewing on its *dedicated*
    /// connection (rmux-sdk handles/owned_session/lease.rs:39, :214-239), which
    /// no request on this terminal's operation or write connections can cause.
    /// It does **not** prove the daemon is gone, and the earlier claim here
    /// that no single poisoned request could cause it was wrong: the lease's
    /// own connection is latch-prone in exactly the same way. `renew_lease_once`
    /// goes through `TransportClient::run_with_deadline`, and one renew
    /// exceeding the deadline calls `abort_with` (rmux-sdk
    /// transport/mod.rs:246-266), whose `set_terminal_failure` is write-once
    /// (transport/state.rs:39-44). Every later renew on that transport then
    /// fails instantly, the retry loop burns its `last_success + ttl` budget on
    /// doomed attempts (lease.rs:176-210), and `lost` flips at TTL. So one
    /// client-side renew timeout — CPU starvation is enough — reports a lost
    /// lease for a session and a daemon that are both healthy, and the sidecar,
    /// which reaps expired leases, then reaps that healthy session for real.
    ///
    /// `false` is not evidence of the opposite either: the heartbeat renews
    /// only every `(ttl/3).max(100ms)` and retries until `last_success + ttl`
    /// (lease.rs:70-77, :176-210), so a genuinely dead sidecar still reads
    /// `false` for up to one lease TTL after it dies.
    fn classify(
        &self,
        operation: &'static str,
        error: rmux_sdk::RmuxError,
    ) -> TerminalBackendError {
        let mapped = TerminalBackendError::from(error);
        if matches!(mapped, TerminalBackendError::ControlPlaneLost) {
            tracing::warn!(
                operation,
                session_id = %self.session_id,
                pane_id = self.reference.pane_id,
                lease_lost = self.owned.lease_lost(),
                "private terminal session control plane was lost"
            );
        }
        mapped
    }

    /// Mints a throwaway pane handle on its own fresh transport.
    ///
    /// The returned handle is for exactly one public operation and is dropped
    /// straight after. If the caller's future is dropped mid-request, the
    /// poison latch described on [`RmuxTerminal`] lands on this connection —
    /// which nothing else will ever use — instead of on a connection shared
    /// with anything, in this session or any other.
    ///
    /// This is a *slot* handle (`session`, window 0, pane 0), the same
    /// [`private_pane_ref`] every other handle in this file addresses. It must
    /// not become a stable-id handle: on a miss, the by-id path fans out
    /// `list-sessions` plus one `list-panes` per session, twice, per call,
    /// which turns a 25 ms poll into O(N) RPCs and can push it past the SDK
    /// deadline -- poisoning the very connection this exists to protect.
    async fn operation_pane(&self, operation: &'static str) -> Result<Pane, TerminalBackendError> {
        self.rmux
            .pane(private_pane_ref(&self.session_name))
            .await
            .map_err(|error| self.classify(operation, error))
    }

    /// The same mint as [`Self::operation_pane`], in the form a detached write
    /// needs: an owned future that borrows nothing.
    ///
    /// It is deliberately a future and not a `Pane`, and deliberately not
    /// `async fn`. The connection must be opened *inside* the spawned task,
    /// after the FIFO permit has been taken, for two reasons that pull the same
    /// way:
    ///
    /// * **Abandonment.** `paste` is polled once and dropped by callers whose
    ///   deadline fires (`driver_io`'s input gate; the actor's turn loop).
    ///   Awaiting the mint on the caller's task would put an await point in
    ///   front of the permit, so a write abandoned on its first poll would
    ///   never be issued at all — silently converting the "may or may not have
    ///   landed" contract into "definitely did not", on the exact path
    ///   `private_abandoned_paste_reaches_the_pane_strictly_before_a_following_
    ///   interrupt` pins.
    /// * **Ordering.** Minting under the permit keeps connection order and
    ///   write order the same statement rather than two that can differ.
    ///
    /// The address is [`private_pane_ref`], the slot form, for the reason
    /// spelled out on [`Self::operation_pane`]: a stable-id handle fans out
    /// O(N) RPCs on a miss.
    fn write_pane(
        &self,
    ) -> impl Future<Output = Result<Pane, rmux_sdk::RmuxError>> + Send + 'static {
        let rmux = Arc::clone(&self.rmux);
        let target = private_pane_ref(&self.session_name);
        async move { rmux.pane(target).await }
    }

    /// [`Self::write_pane`] for the one write addressed at the window.
    ///
    /// THE WINDOW IS THE RESOURCE; THE PANE ONLY INHERITS FROM IT. See
    /// [`TerminalBackend::create`] for the long version and for what a
    /// `resize-pane` on a lone pane silently does instead.
    fn write_window(
        &self,
    ) -> impl Future<Output = Result<Window, rmux_sdk::RmuxError>> + Send + 'static {
        let rmux = Arc::clone(&self.rmux);
        let target = private_window_ref(&self.session_name);
        async move { rmux.window(target).await }
    }

    /// Runs one terminal write on a detached task, in FIFO order with every
    /// other write and with teardown.
    ///
    /// Two separate properties are needed, and each needs its own mechanism:
    ///
    /// 1. **Detach.** `tokio::spawn` returns a `JoinHandle` that *detaches* on
    ///    drop rather than aborting, so when the caller's deadline fires and
    ///    this future is dropped, the rmux request is still driven to
    ///    completion and its connection is never poisoned.
    /// 2. **Ordering.** The `write_order` gate is a hard requirement, not
    ///    politeness. Without it, an abandoned `paste` could still be sitting in
    ///    a spawned task when `spawn_cancel_recovery` sends its `C-c`, and the
    ///    prompt would be typed into the composer *after* the interrupt. The
    ///    permit is acquired on the caller's task (so queue order is caller
    ///    order, not scheduler order) and then moved into the spawned task (so
    ///    it is held for the whole round trip, including after abandonment).
    ///    Acquisition and spawn are not separated by an await point, so the
    ///    permit can never be acquired and then lost to cancellation.
    ///
    /// ## Why this spawn is untracked, and what bounds it instead
    ///
    /// The service's `TrackedTasks` shutdown fence
    /// (`pseudomux_service::tasks`) is the accounting every other detached
    /// pmux task joins. This one cannot: `pseudomux-rmux` is the layer beneath
    /// `pseudomux-service` and does not depend on it, so the permit type is
    /// not nameable here. Introducing an inverted dependency, or a callback
    /// seam through `TerminalBackend`, to reach it would be a larger change
    /// than the property is worth -- because the property is already available
    /// locally, in three parts that hold with no help from above:
    ///
    /// * **Count.** The permit is taken *before* the spawn, so at most one
    ///   detached write per terminal exists at any instant. There is nothing
    ///   to accumulate and therefore nothing to bound.
    /// * **Time.** The request runs under the SDK's configured
    ///   `operation_timeout` (`RmuxBackendConfig::operation_timeout`, applied
    ///   as the facade's `default_timeout`), so an abandoned write completes or
    ///   fails within it instead of running until process exit. Since layer (b)
    ///   the spawned task also opens the connection the write rides
    ///   ([`Self::write_pane`]), and that is bounded by the same number:
    ///   `Rmux::pane` resolves the facade's timeout and hands it to
    ///   `connect_resolved_transport_for_operation` (rmux-sdk
    ///   handles/rmux.rs:172-177). The bound is one deadline wider than it was,
    ///   not unbounded.
    /// * **Teardown.** [`TerminalSession::close`] takes this same permit before
    ///   it does anything, so no terminal is torn down while one of its own
    ///   writes is still in flight, and a service shutdown that closes every
    ///   session therefore cannot complete with a bracketed paste outstanding.
    async fn detached_write<F>(
        &self,
        operation: &'static str,
        write: F,
    ) -> Result<(), TerminalBackendError>
    where
        F: Future<Output = Result<(), rmux_sdk::RmuxError>> + Send + 'static,
    {
        let order = Arc::clone(&self.write_order).lock_owned().await;
        let task = tokio::spawn(async move {
            let _order = order;
            write.await
        });
        match task.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(self.classify(operation, error)),
            Err(_) => Err(TerminalBackendError::Rmux(format!(
                "detached terminal {operation} task did not complete"
            ))),
        }
    }
}

#[async_trait]
impl TerminalSession for RmuxTerminal {
    fn backend_ref(&self) -> &BackendSessionRef {
        &self.reference
    }

    fn lease_lost(&self) -> bool {
        self.owned.lease_lost()
    }

    /// Reads the rendered screen on a throwaway handle.
    ///
    /// This is by far the highest-frequency terminal call in pmux -- a 25 ms
    /// poll for the whole length of every turn -- and it is the call under most
    /// of the censused caller deadlines. A throwaway handle costs one extra
    /// `UnixStream::connect` and one extra `Handshake` RPC, because a snapshot
    /// on a slot handle first asks whether the daemon supports pane-by-id
    /// (rmux-sdk handles/pane/snapshot.rs:68) and the capability cache it
    /// consults is per connection. In exchange, losing the deadline race costs
    /// nothing but the connection that lost it. The trade has not been measured
    /// through pmux, so it is stated as a mechanism and not as a number.
    async fn snapshot(&self) -> Result<TerminalSnapshot, TerminalBackendError> {
        let pane = self.operation_pane("snapshot").await?;
        let snapshot = pane
            .snapshot()
            .await
            .map_err(|error| self.classify("snapshot", error))?;
        Ok(terminal_snapshot(snapshot))
    }

    /// The same capture as [`Self::snapshot`], keeping the cell colours.
    ///
    /// Same throwaway handle, same RPC, same cost: the daemon always sends the
    /// full cell grid, and `PaneSnapshot::visible_text` is a client-side
    /// reduction of it. This does not ask for more from the daemon than a
    /// snapshot does; it declines to throw part of the answer away.
    async fn styled_screen(&self) -> Result<StyledScreen, TerminalBackendError> {
        let pane = self.operation_pane("styled_screen").await?;
        let snapshot = pane
            .snapshot()
            .await
            .map_err(|error| self.classify("styled_screen", error))?;
        Ok(styled_screen(&snapshot))
    }

    /// Waits for visible text on a throwaway handle.
    ///
    /// The SDK implements this as a client-side snapshot poll whose inner
    /// `tokio::time::timeout(remaining, pane.snapshot())` lives at rmux-sdk
    /// wait.rs:333. That drop is *inside* the SDK, so detaching from pmux
    /// cannot reach it and neither can any call-site change. Handing the wait
    /// its own connection is what makes that drop harmless.
    async fn wait_visible_text(
        &self,
        needle: &str,
        timeout: Duration,
    ) -> Result<TerminalSnapshot, TerminalBackendError> {
        if needle.is_empty() {
            return Err(TerminalBackendError::InvalidLaunch(
                "visible text locator must not be empty".into(),
            ));
        }
        let pane = self.operation_pane("wait_visible_text").await?;
        let snapshot = pane
            .expect_visible_text()
            .to_contain(needle)
            .timeout(timeout)
            .await
            .map_err(|error| self.classify("wait_visible_text", error))?;
        Ok(terminal_snapshot(snapshot))
    }

    /// Waits for screen quiescence on a throwaway handle, for the same
    /// rmux-sdk wait.rs:333 reason as [`Self::wait_visible_text`].
    async fn wait_quiet(
        &self,
        stable_for: Duration,
        timeout: Duration,
    ) -> Result<TerminalSnapshot, TerminalBackendError> {
        let pane = self.operation_pane("wait_quiet").await?;
        let snapshot = pane
            .wait_until_stable_for(stable_for)
            .timeout(timeout)
            .await
            .map_err(|error| self.classify("wait_quiet", error))?;
        Ok(terminal_snapshot(snapshot))
    }

    async fn paste(&mut self, text: &str) -> Result<(), TerminalBackendError> {
        // Validation stays on the caller's task on purpose. A detached write is
        // guaranteed to land, so a rejected prompt must be refused before
        // anything is spawned and before an ordering permit is taken.
        let payload = bracketed_paste_payload(text)?;
        let pane = self.write_pane();
        self.detached_write("paste", async move { pane.await?.send_text(payload).await })
            .await
    }

    async fn enter(&mut self) -> Result<(), TerminalBackendError> {
        let pane = self.write_pane();
        self.detached_write("enter", async move { pane.await?.send_key("Enter").await })
            .await
    }

    async fn interrupt(&mut self) -> Result<(), TerminalBackendError> {
        let pane = self.write_pane();
        self.detached_write(
            "interrupt",
            async move { pane.await?.send_key("C-c").await },
        )
        .await
    }

    async fn resize(&mut self, rows: u16, cols: u16) -> Result<(), TerminalBackendError> {
        if rows == 0 || cols == 0 {
            return Err(TerminalBackendError::InvalidLaunch(
                "terminal dimensions must be non-zero".into(),
            ));
        }
        // THE WINDOW IS THE RESOURCE HERE TOO -- the same fact as in
        // [`TerminalBackend::create`], which is where the long version of this
        // is written down. `Pane::resize` becomes `resize-pane -x/-y`, and for
        // a single-pane window that records a `requested_main_width` and then
        // rebuilds the layout tree against the WINDOW's size, so a lone pane
        // cannot exceed its window -- AND THE CALL RETURNS SUCCESS. This site
        // was left on `pane.resize` when `create` was fixed, so every later
        // resize was accepted and silently clamped to the window it was already
        // in, which is the original defect with a different entry point.
        //
        // A fresh transport per call, for the same reason
        // [`RmuxTerminal::operation_pane`] mints one: this handle is for
        // exactly one operation, so a poisoning drop lands on a connection
        // nothing else -- least of all teardown -- will ever use.
        //
        // Minted inside the spawned task like every other write. It used to be
        // awaited here, on the caller's task, which put an await point in front
        // of the FIFO permit: a resize abandoned on its first poll took the
        // connect with it and was never issued.
        let window = self.write_window();
        // Still detached and still inside the write-order gate. It is not one
        // of the twelve censused sites, but it is a write on this session
        // reached from a request handler that can be dropped when its client
        // goes away, and routing it through the same gate keeps "every write is
        // detached and ordered" true without exceptions for the next reader to
        // trip over. Both dimensions travel in one request, so there is no
        // window in which the terminal is half-resized.
        self.detached_write("resize", async move {
            window.await?.resize(Some(cols), Some(rows)).await
        })
        .await
    }

    async fn close(&mut self) -> Result<bool, TerminalBackendError> {
        if self.closed {
            return Ok(true);
        }

        // Teardown joins the same FIFO the detached writes use -- on every
        // close path, not only the first -- so it can neither overtake an
        // abandoned write already in flight nor be overtaken by one issued
        // while the kill round trip is open. This is also the third of the
        // three local bounds described on [`RmuxTerminal::detached_write`]:
        // holding it for the whole call is what makes "no terminal is torn down
        // while one of its own writes is in flight" true without reaching the
        // service's task accounting, which this layer cannot see.
        //
        // Best-effort and bounded on purpose: cleanup is the last thing
        // standing between an abandoned session and a surviving interactive
        // process, so a wedged write must be able to delay it but never to
        // prevent it.
        let _write_order = tokio::time::timeout(
            self.cleanup_timeout,
            Arc::clone(&self.write_order).lock_owned(),
        )
        .await
        .ok();

        if !self.cleanup_requested {
            let observation = self
                .boundary
                .observe()
                .await
                .map_err(process_boundary_error)?;
            self.escaped_descendant_observed |= observation.escaped_descendant_observed();

            // Observe the process tree while cleanup is in flight.  Otherwise
            // a child could be seen before teardown, reparent/session-escape
            // during the rmux round trip, and be forgotten by a post-ack ps
            // snapshot.
            let cleanup = observe_cleanup_request(&mut self.owned, &mut self.boundary).await;
            self.escaped_descendant_observed |= cleanup.escaped_descendant_observed;
            // The kill request was driven to completion either way, so record
            // that before branching on anything else. Leaving this false is
            // what the old early return did, and it would tell a later retry to
            // issue a second kill for a request this call already delivered.
            self.cleanup_requested = true;
            if cleanup.observation_failed {
                // Deliberately not a control-flow decision. A missing
                // process-table sample is inconclusive by definition, and every
                // path below re-observes the boundary and ends in `force_reap`,
                // which answers the same question with evidence instead of with
                // an error. It is logged because a sample failure that is then
                // superseded by a successful reap would otherwise leave no
                // trace anywhere that it happened.
                tracing::warn!(
                    operation = "close",
                    session_id = %self.session_id,
                    pane_id = self.reference.pane_id,
                    "process-table sample failed while private terminal cleanup was in flight"
                );
            }
            if let Err(error) = cleanup.result {
                // The request may have committed before its response was
                // lost. The local boundary remains authoritative and can
                // safely reap exact members even when rmux is unavailable, so
                // the mapped error is recorded rather than propagated.
                let _ = self.classify("close", error);
                if !self
                    .boundary
                    .force_reap(self.cleanup_timeout)
                    .await
                    .map_err(process_boundary_error)?
                {
                    return Ok(false);
                }
                self.escaped_descendant_observed |= self.boundary.escaped_descendant_observed();
                if self.escaped_descendant_observed {
                    return Ok(false);
                }
                self.closed = true;
                return Ok(true);
            }
        }

        // rmux first sends HUP and acknowledges session removal before its
        // background teardown is necessarily reaped. Give that path a short
        // grace period, then kill any exact members still proven to belong to
        // this isolated POSIX session.
        let grace = CONTROL_PLANE_CLEANUP_GRACE.min(self.cleanup_timeout);
        let mut process_reaped = self
            .boundary
            .wait_until_reaped(grace)
            .await
            .map_err(process_boundary_error)?;
        self.escaped_descendant_observed |= self.boundary.escaped_descendant_observed();
        if !process_reaped && !self.escaped_descendant_observed {
            process_reaped = self
                .boundary
                .force_reap(self.cleanup_timeout)
                .await
                .map_err(process_boundary_error)?;
            self.escaped_descendant_observed |= self.boundary.escaped_descendant_observed();
        }
        if process_reaped && !self.escaped_descendant_observed {
            self.closed = true;
            Ok(true)
        } else {
            // An rmux kill acknowledgement is not a reap acknowledgement.
            // False keeps the higher-level session fail-closed and retryable.
            Ok(false)
        }
    }
}

/// The exact bytes pmux writes to inject one prompt, or a refusal.
///
/// # The whole prompt is inside one bracketed paste, so the terminator is the
/// only thing that can end it early
///
/// A terminal ends a bracketed paste at the first `\e[201~` it sees. A reader
/// scanning for that terminator — which is what a real consumer does; see
/// `read_bracketed_paste` in `crates/e2e/src/bin/pmux-test-claude.rs` — returns
/// everything before it as pasted text and leaves the remainder in the input
/// stream. That remainder is then read as KEYSTROKES.
///
/// For a caller-supplied prompt containing `\e[201~`, that is a caller-controlled
/// wrong-answer path with a very short fuse: the composer it lands in is one `/`
/// away from a live command menu whose entries include `/logout`, `/exit`,
/// `/config` and `/clear`. Nothing downstream of this function can undo it,
/// because by then the bytes are keystrokes and pmux has no way to tell them
/// from a user's.
///
/// So the refusal is here, and it is over-broad on purpose: any ESC at all, not
/// `\e[201~` specifically. Matching the terminator exactly would be a filter
/// over an encoding with more than one spelling — 8-bit C1 CSI, a split write, a
/// terminal in a mode this code does not model — and every one of those is a
/// bug that only appears against a real Claude Code. A prompt with an ESC in it
/// has no legitimate meaning through this channel, so there is nothing to trade
/// away by refusing all of them. NUL is refused with it because it is the other
/// byte that terminates a C string somewhere below this layer.
///
/// This is deliberately a second, independent statement of the guard
/// `pseudomux_service::driver_io::validate_prompt` already makes. That one is a
/// policy filter over caller bytes and can be relaxed; this one is the wire
/// format's own precondition and must not be. A future relaxation of the policy
/// filter therefore cannot open this path, which is the only property worth
/// having here.
pub fn bracketed_paste_payload(text: &str) -> Result<String, TerminalBackendError> {
    if text.contains(['\0', '\u{1b}']) {
        return Err(TerminalBackendError::InvalidLaunch(
            "prompt contains NUL or ESC".into(),
        ));
    }
    Ok(format!("\u{1b}[200~{text}\u{1b}[201~"))
}

fn terminal_snapshot(snapshot: rmux_sdk::PaneSnapshot) -> TerminalSnapshot {
    let cursor = snapshot.cursor;
    TerminalSnapshot {
        revision: snapshot.revision,
        rows: snapshot.rows,
        cols: snapshot.cols,
        cursor: Some(TerminalCursor {
            row: cursor.row,
            col: cursor.col,
            visible: cursor.visible,
            style: cursor.style,
        }),
        visible_text: snapshot.visible_text(),
    }
}

fn styled_screen(snapshot: &rmux_sdk::PaneSnapshot) -> StyledScreen {
    let cursor = snapshot.cursor;
    let cols = usize::from(snapshot.cols);
    let cells = (0..snapshot.rows)
        .map(|row| {
            let start = usize::from(row).saturating_mul(cols);
            let end = start.saturating_add(cols).min(snapshot.cells.len());
            if cols == 0 || start >= snapshot.cells.len() {
                return Vec::new();
            }
            snapshot.cells[start..end]
                .iter()
                .map(|cell| StyledCell {
                    text: cell.text().to_owned(),
                    foreground: cell.foreground.into(),
                    padding: cell.is_padding(),
                })
                .collect()
        })
        .collect();
    StyledScreen {
        revision: snapshot.revision,
        rows: snapshot.rows,
        cols: snapshot.cols,
        cursor: Some(TerminalCursor {
            row: cursor.row,
            col: cursor.col,
            visible: cursor.visible,
            style: cursor.style,
        }),
        cells,
    }
}

/// Everything one observed `owned.cleanup()` produced.
///
/// A struct rather than a `Result` because the two axes are independent: the
/// rmux request has an outcome, and the local process boundary separately may
/// or may not have been sampleable while that request was open. Collapsing them
/// into one `Result` is what made the old signature able to lose the first.
struct CleanupObservation {
    /// What rmux said. Always present: the request is driven to completion even
    /// when observation fails, so there is no case in which the answer is
    /// unknown because it was thrown away.
    result: Result<bool, rmux_sdk::RmuxError>,
    /// Escape evidence accumulated by the samples taken while the request was
    /// in flight, read out of the boundary before returning.
    escaped_descendant_observed: bool,
    /// A process-table sample failed during the flight. Reported for the record
    /// only; on its own it proves nothing about the boundary.
    observation_failed: bool,
}

/// Drives one `owned.cleanup()` to completion while keeping the local process
/// boundary under observation.
///
/// The `?` that used to sit on `boundary.observe()` here was its own
/// cancellation-poison site, and the least visible one in the codebase. A
/// failed process-table sample returned immediately, which dropped the pinned
/// in-flight `owned.cleanup()` and — by rmux-sdk
/// transport/cancellation.rs:27-34 → transport/mod.rs:268-271 →
/// transport/state.rs:39-44 — permanently killed the transport. The error then
/// propagated straight out of `close()`, leaving `cleanup_requested` false and
/// never reaching the `force_reap` fallback, so the caller learned neither that
/// a kill had been delivered nor that the control plane had just been
/// destroyed.
///
/// Both halves are fixed, and this function can no longer fail. The request is
/// always driven to completion, and all three facts it produced — what rmux
/// said, what the boundary saw, and whether a sample went missing — are
/// returned for `close` to fold into its own state.
///
/// A missing sample deliberately no longer short-circuits anything. It is
/// inconclusive by construction, and both paths `close` can take from here
/// re-observe the boundary and end in `OwnedProcessBoundary::force_reap`, which
/// itself propagates a persistent process-table failure as an error. So the old
/// early return has been replaced by something strictly stronger, not merely by
/// something quieter: a transient failure now recovers with real evidence, and
/// a permanent one still fails closed with an error of the same class.
///
/// The final await is bounded by the SDK's own operation deadline on the
/// cleanup request, and `close()` itself is detached by its caller, so waiting
/// here cannot stall an actor.
async fn observe_cleanup_request(
    owned: &mut OwnedSession,
    boundary: &mut OwnedProcessBoundary,
) -> CleanupObservation {
    let cleanup = owned.cleanup();
    tokio::pin!(cleanup);
    let mut observation_failed = false;
    let result = loop {
        let sampled = tokio::select! {
            result = &mut cleanup => break result,
            () = tokio::time::sleep(PROCESS_OBSERVATION_POLL_INTERVAL) => {
                boundary.observe().await
            }
        };
        if sampled.is_err() {
            // Stop sampling, but never abandon the request: returning here is
            // exactly what poisoned the transport. Drive it to completion and
            // report the missing sample alongside its result.
            observation_failed = true;
            break (&mut cleanup).await;
        }
    };
    CleanupObservation {
        result,
        escaped_descendant_observed: boundary.escaped_descendant_observed(),
        observation_failed,
    }
}

fn process_boundary_error(error: crate::ProcessBoundaryError) -> TerminalBackendError {
    TerminalBackendError::ProcessBoundary(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pane whose row 0 mixes a wide glyph, its padding cell, an explicitly
    /// coloured run and trailing blanks -- every case `render_cells_lossy` has
    /// to agree with rmux about.
    fn shaped_pane() -> rmux_sdk::PaneSnapshot {
        fn cell(text: &str, colour: rmux_sdk::PaneColor) -> rmux_sdk::PaneCell {
            rmux_sdk::PaneCell {
                glyph: rmux_sdk::PaneGlyph::new(text, 1),
                attributes: rmux_sdk::PaneAttributes::EMPTY,
                foreground: colour,
                background: rmux_sdk::PaneColor::Default,
                underline: rmux_sdk::PaneColor::Default,
            }
        }
        let mut cells = vec![
            cell("❯", rmux_sdk::PaneColor::Default),
            cell(" ", rmux_sdk::PaneColor::Default),
        ];
        for glyph in "/clear".chars() {
            cells.push(cell(&glyph.to_string(), rmux_sdk::PaneColor::indexed(153)));
        }
        cells.push(rmux_sdk::PaneCell::padding());
        while cells.len() < 16 {
            cells.push(rmux_sdk::PaneCell::blank());
        }
        for glyph in "─".repeat(16).chars() {
            cells.push(cell(&glyph.to_string(), rmux_sdk::PaneColor::Default));
        }
        rmux_sdk::PaneSnapshot::new(
            16,
            2,
            cells,
            rmux_sdk::PaneCursor {
                row: 0,
                col: 8,
                visible: true,
                style: 0,
            },
        )
        .unwrap()
        .with_revision(7)
    }

    /// The two views of one capture must never disagree about its text. Every
    /// input gate settles on `TerminalSnapshot::visible_text`, and the pre-Enter
    /// selection proof reads its rows from `StyledScreen`; if these two
    /// renderings drifted, the proof would be describing a different screen from
    /// the one that was proven stable.
    #[test]
    fn styled_text_matches_lossy_pane_text() {
        let pane = shaped_pane();
        let styled = styled_screen(&pane);
        assert_eq!(styled.visible_text(), pane.visible_text());
        assert_eq!(styled.row_text(0), pane.row_text(0));
        assert_eq!(styled.row_text(1), pane.row_text(1));
        assert_eq!(styled.to_terminal_snapshot(), terminal_snapshot(pane));
    }

    /// The colours the plain-text view drops are present here, and a palette
    /// entry rebuilt by index is the same identity token as a captured one.
    #[test]
    fn styled_cells_carry_the_colour_the_text_view_discards() {
        let styled = styled_screen(&shaped_pane());
        let row = styled.row(0);
        assert_eq!(row[0].foreground, CellColor::Unstyled);
        assert_eq!(row[2].foreground, CellColor::indexed(153));
        assert!(CellColor::indexed(153).is_styled());
        assert!(!CellColor::Unstyled.is_styled());
        assert_ne!(CellColor::indexed(153), CellColor::indexed(246));
        assert!(row[8].is_padding());
    }

    #[test]
    fn private_transport_loss_remains_typed_across_the_terminal_boundary() {
        let transport = rmux_sdk::RmuxError::transport(
            "snapshot exact private pane",
            std::io::Error::new(std::io::ErrorKind::ConnectionReset, "private diagnostic"),
        );
        assert!(matches!(
            TerminalBackendError::from(transport),
            TerminalBackendError::ControlPlaneLost
        ));
    }

    #[test]
    fn private_session_names_are_generation_bound() {
        let session_id = Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").unwrap();
        let first = private_session_name(session_id).unwrap().to_string();
        let second = private_session_name(session_id).unwrap().to_string();
        assert_ne!(first, second);
        assert!(first.contains("0123456789abcdef0123456789abcdef"));
        assert!(second.contains("0123456789abcdef0123456789abcdef"));
    }

    #[test]
    fn every_private_handle_addresses_the_same_pane_slot() {
        let name = SessionName::new("pmux-slot-agreement").unwrap();
        let reference = private_pane_ref(&name);
        assert_eq!(reference.session_name, name);
        assert_eq!(reference.window_index, PRIVATE_PANE_SLOT.0);
        assert_eq!(reference.pane_index, PRIVATE_PANE_SLOT.1);
    }

    fn unreachable_backend(socket: PathBuf) -> RmuxBackendConfig {
        RmuxBackendConfig {
            socket,
            launcher: PathBuf::from("/nonexistent/pmux-launcher"),
            launcher_socket: PathBuf::from("/nonexistent/launcher.sock"),
            operation_timeout: Duration::from_millis(500),
            lease_ttl: Duration::from_secs(5),
        }
    }

    /// `configure` must be inert. If it ever contacts the daemon again, the
    /// startup probe below stops being the reachability check and
    /// `PrivateRuntime::start` acquires a second, undeclared one.
    #[test]
    fn configuring_a_backend_contacts_nothing_and_still_refuses_relative_endpoints() {
        let unreachable = PathBuf::from("/nonexistent/pmux-configure-probe/rmux.sock");
        assert!(!unreachable.exists());
        RmuxBackend::configure(unreachable_backend(unreachable))
            .map(|_| ())
            .expect("configuration must not require a reachable daemon");

        let relative = RmuxBackend::configure(unreachable_backend(PathBuf::from("rmux.sock")));
        assert!(matches!(
            relative.map(|_| ()),
            Err(TerminalBackendError::InvalidLaunch(_))
        ));
    }

    /// The startup reachability check that `.connect()` used to provide.
    ///
    /// A bad socket must fail at start, not at the first session.
    #[tokio::test]
    async fn the_startup_probe_refuses_an_unreachable_socket() {
        let root = tempfile::Builder::new()
            .prefix("pmux-probe-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = root.path().join("absent-rmux.sock");
        assert!(!socket.exists());
        let backend = RmuxBackend::configure(unreachable_backend(socket)).unwrap();
        let probed = backend.probe_control_plane().await;
        assert!(
            matches!(probed, Err(TerminalBackendError::ControlPlaneLost)),
            "an unreachable socket must fail the startup probe, got {probed:?}"
        );
    }

    /// A path that accepts connections but never speaks the rmux wire protocol
    /// passed the old `.connect()` check, which only opened the socket. The
    /// handshake probe is strictly stronger and must reject it.
    #[tokio::test]
    async fn the_startup_probe_refuses_a_socket_that_is_not_an_rmux_daemon() {
        let root = tempfile::Builder::new()
            .prefix("pmux-probe-mute-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = root.path().join("mute.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        // Accept and then hold the connection open without answering, so the
        // failure is the absent handshake response and not a closed socket.
        let accepting = tokio::spawn(async move {
            let mut accepted = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                accepted.push(stream);
            }
        });

        let backend = RmuxBackend::configure(unreachable_backend(socket)).unwrap();
        let probed = backend.probe_control_plane().await;
        accepting.abort();
        assert!(
            matches!(probed, Err(TerminalBackendError::ControlPlaneLost)),
            "a socket that never completes the rmux handshake must fail the startup probe, got {probed:?}"
        );
    }

    /// The health probe's two measured faults must arrive as two different
    /// values, because the operator response differs: a socket nobody is
    /// listening on is a dead sidecar, and a socket that accepts and then never
    /// answers is a live sidecar that has stopped serving.
    ///
    /// `TerminalBackendError` flattens both into `ControlPlaneLost`, which is
    /// right for a session and useless for a report. This is the check that the
    /// distinction survives.
    #[tokio::test]
    async fn the_request_path_probe_separates_an_absent_daemon_from_a_silent_one() {
        let root = tempfile::Builder::new()
            .prefix("pmux-fault-classify-")
            .tempdir_in("/tmp")
            .unwrap();

        let absent = root.path().join("absent-rmux.sock");
        assert!(!absent.exists());
        let backend = RmuxBackend::configure(unreachable_backend(absent)).unwrap();
        assert_eq!(
            backend.probe_request_path().await,
            Err(ControlPlaneFault::Unreachable)
        );

        // Accepts and never answers, which is what a `SIGSTOP`ped sidecar looks
        // like from this side: the socket is bound and the kernel completes the
        // connection, and no reply ever comes.
        let silent = root.path().join("silent.sock");
        let listener = tokio::net::UnixListener::bind(&silent).unwrap();
        let accepting = tokio::spawn(async move {
            let mut accepted = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                accepted.push(stream);
            }
        });
        let backend = RmuxBackend::configure(unreachable_backend(silent)).unwrap();
        let probed = backend.probe_request_path().await;
        accepting.abort();
        assert_eq!(probed, Err(ControlPlaneFault::Unresponsive));
    }

    /// A peer that accepts and then dies mid-exchange is reported
    /// [`ControlPlaneFault::Unreachable`], the same as a socket nobody is
    /// listening on.
    ///
    /// THIS IS THE DOC COMMENT'S CHECK, not an aspiration about the enum. The
    /// variant's doc used to read "No usable connection to the private socket.
    /// Nothing was dispatched." -- a promise strictly stronger than the
    /// predicate, which is the residual arm of every non-`TimedOut`
    /// `io::ErrorKind`. MEASURED by this test: the connection is established,
    /// the request IS dispatched, and the SDK's own operation name says so --
    /// `Transport { operation: "complete \`list-sessions\` request/response
    /// exchange with rmux daemon", source: Custom { kind: BrokenPipe, error:
    /// "Broken pipe (os error 32)" } }` -- and the answer is still
    /// `Unreachable`.
    ///
    /// The doc was narrowed rather than the classifier widened, and this exists
    /// so that choice cannot rot back into a claim nobody tests: if a later
    /// change gives mid-exchange death its own fault, this test fails and the
    /// doc has to move with it.
    #[tokio::test]
    async fn a_peer_that_dies_mid_exchange_is_reported_unreachable_like_an_absent_one() {
        let root = tempfile::Builder::new()
            .prefix("pmux-fault-midexchange-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = root.path().join("dies.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        // Accepts and immediately hangs up. Unlike the silent peer above, which
        // holds the connection open until the deadline, this one completes the
        // connection and then breaks it under an exchange already in flight.
        let accepting = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });

        let backend = RmuxBackend::configure(unreachable_backend(socket)).unwrap();
        let probed = backend.probe_request_path().await;
        accepting.abort();
        assert_eq!(
            probed,
            Err(ControlPlaneFault::Unreachable),
            "a mid-exchange hangup is classified by the residual arm; if this ever stops being \
             `Unreachable`, `ControlPlaneFault::Unreachable`'s doc comment is wrong again"
        );
    }
}
