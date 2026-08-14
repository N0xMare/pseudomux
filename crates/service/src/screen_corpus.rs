//! A recorded corpus of Claude Code screens, and the invariants that must hold
//! over every frame in it.
//!
//! # Why this exists
//!
//! Path B drives the real Claude Code TUI, so every input gate and every
//! completion gate in pmux is a statement about terminal geometry that Claude
//! Code can change without notice. Two such statements have already been wrong,
//! and neither was findable by reading the code:
//!
//! * The composer gate measured the cursor's distance from the physical bottom
//!   of the grid. `snapshot.rows - cursor.row - 1 > 4` reads as obviously
//!   correct and survived four static review rounds; it is wrong only once you
//!   know Ink does not always paint to the bottom, which is a fact about a
//!   running program and not about this source file.
//! * The `/clear` menu renders its selection in FOREGROUND COLOUR ALONE, and the
//!   plain-text snapshot discarded the cell grid. The highlight was not hard to
//!   read in pmux's data, it was absent from it.
//!
//! Both died to a live screen capture. Live captures cost real Claude turns and
//! real wall-clock, and pmux already takes hundreds of them per session — every
//! 25 ms for the length of every turn — and then throws all of them away. This
//! module keeps them.
//!
//! # What a corpus is
//!
//! An append-only NDJSON file. The first line is a [`CorpusStamp`] naming the
//! Claude Code version, OS, arch and pane geometry the frames were taken under;
//! every subsequent line is one [`CorpusFrame`]. Provenance is on the file
//! because the invariants below are claims about a *version* of Claude Code, and
//! a frame with no version attached cannot refute or confirm any of them.
//!
//! # Recording is opt-in and must not perturb the production path
//!
//! Recording is off unless `PMUX_SCREEN_CORPUS_DIR` is set, and the disabled
//! path is a single relaxed atomic load with no allocation: [`record_snapshot`]
//! takes the frame by reference and only clones it once a recorder exists.
//! When enabled, frames go to a bounded channel drained by one dedicated OS
//! thread — never the tokio runtime the 25 ms poll shares — and a full channel
//! DROPS the frame rather than blocking the poll. A corpus that lost frames is a
//! smaller corpus; a poll that blocked on a corpus write is a changed
//! measurement, and the whole point of the recording is that it measures what
//! production actually saw. [`dropped_frames`] reports the loss so a recording
//! run can say how complete it was.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::time::{SystemTime, UNIX_EPOCH};

use pseudomux_rmux::{CellColor, StyledCell, StyledScreen, TerminalCursor, TerminalSnapshot};
use serde::{Deserialize, Serialize};

use crate::driver_io::{TerminalScreenState, classify_terminal_snapshot, screen_geometry};

/// Environment variable that turns recording on. Its value is a directory.
pub const CORPUS_DIR_ENV: &str = "PMUX_SCREEN_CORPUS_DIR";
/// Optional free-text label stamped onto the recording, e.g. `clear-menu`.
pub const CORPUS_LABEL_ENV: &str = "PMUX_SCREEN_CORPUS_LABEL";
/// Optional Claude Code version stamp. Recorded verbatim; pmux does not shell
/// out to `claude --version` from inside a session to find it out, because that
/// would be a process spawn on the production path.
pub const CORPUS_CLAUDE_VERSION_ENV: &str = "PMUX_SCREEN_CORPUS_CLAUDE_VERSION";

/// Corpus schema version. Bump when a frame's meaning changes, never when a
/// field is added: a reader must be able to load an older corpus, because the
/// point of a corpus is that it outlives the code that recorded it.
pub const CORPUS_SCHEMA: u32 = 1;

/// How many frames may be queued for the writer thread before frames are
/// dropped. At a 25 ms poll this is ~100 seconds of one session's backlog, which
/// a local append will never fall behind by; it is a bound, not a budget.
const RECORDER_QUEUE_DEPTH: usize = 4096;

/// Provenance for a recording. Every invariant in this module is a claim about
/// one Claude Code version's rendering, so a frame without this is evidence of
/// nothing.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CorpusStamp {
    pub schema: u32,
    /// Verbatim `claude --version` output, or `"unknown"`.
    pub claude_version: String,
    pub os: String,
    pub arch: String,
    /// Pane geometry the session was launched at. Individual frames carry their
    /// own `rows`/`cols`, which differ from these after a resize.
    pub rows: u16,
    pub cols: u16,
    pub recorded_unix_ms: u64,
    pub label: String,
}

impl CorpusStamp {
    /// A stamp for the current host, taking the Claude version from the
    /// environment because the recording process must not spawn anything.
    #[must_use]
    pub fn for_host(rows: u16, cols: u16, label: impl Into<String>) -> Self {
        Self {
            schema: CORPUS_SCHEMA,
            claude_version: std::env::var(CORPUS_CLAUDE_VERSION_ENV)
                .unwrap_or_else(|_| "unknown".to_owned()),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            rows,
            cols,
            recorded_unix_ms: unix_ms(),
            label: label.into(),
        }
    }
}

/// One recorded cell. Mirrors [`StyledCell`] rather than serializing it, so the
/// on-disk shape is owned here and a change to the rmux type is a compile error
/// in one conversion instead of a silently reinterpreted corpus.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CorpusCell {
    pub text: String,
    /// `None` is [`CellColor::Unstyled`]; `Some` carries rmux's own opaque
    /// encoding, compared and never interpreted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreground: Option<i32>,
    /// Whether this is the trailing half of a double-width glyph.
    #[serde(default, skip_serializing_if = "is_false")]
    pub padding: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl CorpusCell {
    fn from_cell(cell: &StyledCell) -> Self {
        Self {
            text: cell.text.clone(),
            foreground: match cell.foreground {
                CellColor::Unstyled => None,
                CellColor::Explicit(encoded) => Some(encoded),
            },
            padding: cell.is_padding(),
        }
    }

    fn to_cell(&self) -> StyledCell {
        let foreground = self
            .foreground
            .map_or(CellColor::Unstyled, CellColor::Explicit);
        if self.padding {
            StyledCell::padding(self.text.clone(), foreground)
        } else {
            StyledCell::new(self.text.clone(), foreground)
        }
    }
}

/// A recorded cursor. Mirrors [`TerminalCursor`] for the same reason
/// [`CorpusCell`] mirrors [`StyledCell`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CorpusCursor {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
    pub style: u32,
}

impl From<TerminalCursor> for CorpusCursor {
    fn from(value: TerminalCursor) -> Self {
        Self {
            row: value.row,
            col: value.col,
            visible: value.visible,
            style: value.style,
        }
    }
}

impl From<CorpusCursor> for TerminalCursor {
    fn from(value: CorpusCursor) -> Self {
        Self {
            row: value.row,
            col: value.col,
            visible: value.visible,
            style: value.style,
        }
    }
}

/// One recorded frame.
///
/// The two variants are the two distinct reads production performs, kept
/// distinct on disk. A `Styled` frame can produce a `Snapshot` view through
/// [`StyledScreen::to_terminal_snapshot`], but the reverse is not true and
/// collapsing them would silently throw away the cell colours that the `/clear`
/// selection proof is the only consumer of — the exact loss that made the menu
/// highlight absent from pmux's data in the first place.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CorpusFrame {
    Snapshot {
        /// Which production read produced this, e.g. `input_gate`.
        site: String,
        captured_unix_ms: u64,
        revision: u64,
        rows: u16,
        cols: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<CorpusCursor>,
        visible_text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_ready: Option<bool>,
    },
    Styled {
        site: String,
        captured_unix_ms: u64,
        revision: u64,
        rows: u16,
        cols: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cursor: Option<CorpusCursor>,
        /// Row-major, `rows` entries. A short row is preserved rather than
        /// padded, exactly as [`StyledScreen`] preserves it.
        cells: Vec<Vec<CorpusCell>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_ready: Option<bool>,
    },
}

impl CorpusFrame {
    #[must_use]
    pub fn from_snapshot(site: &str, snapshot: &TerminalSnapshot) -> Self {
        Self::Snapshot {
            site: site.to_owned(),
            captured_unix_ms: unix_ms(),
            revision: snapshot.revision,
            rows: snapshot.rows,
            cols: snapshot.cols,
            cursor: snapshot.cursor.map(CorpusCursor::from),
            visible_text: snapshot.visible_text.clone(),
            // Never set at record time. Production does not know ground truth
            // about the screen it is looking at -- that is the whole reason the
            // classifier exists -- so a recorder that stamped an expectation
            // would only be recording its own verdict.
            expect_ready: None,
        }
    }

    #[must_use]
    pub fn from_styled(site: &str, screen: &StyledScreen) -> Self {
        Self::Styled {
            site: site.to_owned(),
            captured_unix_ms: unix_ms(),
            revision: screen.revision,
            rows: screen.rows,
            cols: screen.cols,
            cursor: screen.cursor.map(CorpusCursor::from),
            cells: (0..screen.rows)
                .map(|row| screen.row(row).iter().map(CorpusCell::from_cell).collect())
                .collect(),
            expect_ready: None,
        }
    }

    #[must_use]
    pub fn site(&self) -> &str {
        match self {
            Self::Snapshot { site, .. } | Self::Styled { site, .. } => site,
        }
    }

    /// The verdict this frame is independently known to deserve, if anyone
    /// established one.
    ///
    /// # Why the corpus needs this at all
    ///
    /// Every other invariant in this module is CONDITIONAL on the classifier's
    /// own verdict: "a frame classified `Ready` has exactly two rendered rows
    /// below the cursor". A classifier that stops returning `Ready` satisfies
    /// all of them by having no cases left, and MEASURED, that is precisely what
    /// the composer bug did -- the post-`/clear` frame simply became `Unknown`
    /// and every conditional check went vacuous. Replaying the corpus through
    /// the broken classifier passed.
    ///
    /// An expectation is the unconditional half. It is set by hand, only where a
    /// human or a measurement established the answer WITHOUT consulting the
    /// classifier, and it is what turns "no invariant applies" into "a frame
    /// that must be `Ready` is not".
    #[must_use]
    pub const fn expect_ready(&self) -> Option<bool> {
        match self {
            Self::Snapshot { expect_ready, .. } | Self::Styled { expect_ready, .. } => {
                *expect_ready
            }
        }
    }

    /// The plain-text view every gate but the selection proof classifies.
    ///
    /// A `Styled` frame answers this by rendering its cells through
    /// [`StyledScreen::to_terminal_snapshot`], which is the same reduction
    /// production applies, so replaying either variant classifies exactly what
    /// production classified.
    #[must_use]
    pub fn to_terminal_snapshot(&self) -> TerminalSnapshot {
        match self {
            Self::Snapshot {
                revision,
                rows,
                cols,
                cursor,
                visible_text,
                ..
            } => TerminalSnapshot {
                revision: *revision,
                rows: *rows,
                cols: *cols,
                cursor: cursor.map(TerminalCursor::from),
                visible_text: visible_text.clone(),
            },
            Self::Styled { .. } => self
                .to_styled_screen()
                .expect("a Styled frame always renders a styled screen")
                .to_terminal_snapshot(),
        }
    }

    /// The cell grid, for `Styled` frames only.
    #[must_use]
    pub fn to_styled_screen(&self) -> Option<StyledScreen> {
        match self {
            Self::Snapshot { .. } => None,
            Self::Styled {
                revision,
                rows,
                cols,
                cursor,
                cells,
                ..
            } => Some(StyledScreen::new(
                *revision,
                *rows,
                *cols,
                cursor.map(TerminalCursor::from),
                cells
                    .iter()
                    .map(|row| row.iter().map(CorpusCell::to_cell).collect())
                    .collect(),
            )),
        }
    }
}

/// A loaded corpus: one stamp and the frames recorded under it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Corpus {
    pub stamp: CorpusStamp,
    pub frames: Vec<CorpusFrame>,
    /// Where it was loaded from, so a failing invariant can name the file.
    pub source: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum CorpusError {
    #[error("corpus {path} could not be read: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("corpus {path} is empty; the first line must be the stamp")]
    Empty { path: PathBuf },
    #[error("corpus {path} line {line} is malformed: {source}")]
    Malformed {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "corpus {path} is schema {found}, this build reads schema {expected}; \
         a reader must never reinterpret a corpus it does not understand"
    )]
    Schema {
        path: PathBuf,
        found: u32,
        expected: u32,
    },
}

impl Corpus {
    /// Reads one NDJSON corpus file.
    ///
    /// Blank lines are skipped so a hand-edited or concatenated corpus loads;
    /// a malformed non-blank line is an error, never a skip. Silently dropping
    /// a frame a reader could not parse would turn a schema change into a
    /// quietly shrinking corpus that keeps passing.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CorpusError> {
        let path = path.as_ref();
        let file = File::open(path).map_err(|source| CorpusError::Io {
            path: path.to_owned(),
            source,
        })?;
        let mut lines = BufReader::new(file).lines().enumerate();
        let stamp = loop {
            let Some((index, line)) = lines.next() else {
                return Err(CorpusError::Empty {
                    path: path.to_owned(),
                });
            };
            let line = line.map_err(|source| CorpusError::Io {
                path: path.to_owned(),
                source,
            })?;
            if line.trim().is_empty() {
                continue;
            }
            break serde_json::from_str::<CorpusStamp>(&line).map_err(|source| {
                CorpusError::Malformed {
                    path: path.to_owned(),
                    line: index + 1,
                    source,
                }
            })?;
        };
        if stamp.schema != CORPUS_SCHEMA {
            return Err(CorpusError::Schema {
                path: path.to_owned(),
                found: stamp.schema,
                expected: CORPUS_SCHEMA,
            });
        }

        let mut frames = Vec::new();
        for (index, line) in lines {
            let line = line.map_err(|source| CorpusError::Io {
                path: path.to_owned(),
                source,
            })?;
            if line.trim().is_empty() {
                continue;
            }
            frames.push(
                serde_json::from_str(&line).map_err(|source| CorpusError::Malformed {
                    path: path.to_owned(),
                    line: index + 1,
                    source,
                })?,
            );
        }
        Ok(Self {
            stamp,
            frames,
            source: path.to_owned(),
        })
    }

    /// Loads every `*.ndjson` under `dir`, sorted by file name so a failure
    /// reproduces in the same order on every host. A missing directory yields no
    /// corpora rather than an error: a checkout with no recordings is a valid
    /// state, and the caller decides whether that is acceptable.
    pub fn load_dir(dir: impl AsRef<Path>) -> Result<Vec<Self>, CorpusError> {
        let dir = dir.as_ref();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(Vec::new());
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "ndjson"))
            .collect();
        paths.sort();
        paths.into_iter().map(Self::load).collect()
    }
}

/// Appends frames to one NDJSON corpus file.
pub struct CorpusWriter {
    file: File,
}

impl CorpusWriter {
    /// Creates a new corpus file and writes its stamp as line 1.
    pub fn create(path: impl AsRef<Path>, stamp: &CorpusStamp) -> std::io::Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)?;
        writeln!(file, "{}", serde_json::to_string(stamp)?)?;
        Ok(Self { file })
    }

    pub fn append(&mut self, frame: &CorpusFrame) -> std::io::Result<()> {
        writeln!(self.file, "{}", serde_json::to_string(frame)?)
    }
}

// ---------------------------------------------------------------------------
// Invariants
// ---------------------------------------------------------------------------

/// MEASURED on Claude Code 2.1.70 (five checked-in fixtures) and 2.1.220 (85 of
/// 85 live empty-composer screens): a ready composer renders exactly two rows
/// below itself, a rule and the footer.
///
/// The production gate admits four — 2x the measurement — because a constant
/// that is exactly the measurement refuses the first frame after any footer
/// change. This corpus check asserts the measurement itself, so a Claude release
/// that starts rendering three or five footer rows is reported here as a change
/// to reconcile, while production keeps working. That is the whole division of
/// labour: production is tolerant, the corpus is exact.
pub const MEASURED_RENDERED_ROWS_BELOW_READY_COMPOSER: u16 = 2;

/// A violated corpus invariant, naming the frame precisely enough to reproduce.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvariantViolation {
    /// Index of the offending frame within its corpus.
    pub frame: usize,
    pub site: String,
    pub invariant: &'static str,
    pub detail: String,
}

impl std::fmt::Display for InvariantViolation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "frame {} (site {}): {} -- {}",
            self.frame, self.site, self.invariant, self.detail
        )
    }
}

/// Runs every frame-local invariant over one frame.
///
/// These are the hand-derived calibrations from the geometry study, restated as
/// checks that run forever instead of conclusions recorded once in a comment.
#[must_use]
pub fn check_frame(index: usize, frame: &CorpusFrame) -> Vec<InvariantViolation> {
    let snapshot = frame.to_terminal_snapshot();
    let state = classify_terminal_snapshot(&snapshot);
    let geometry = screen_geometry(&snapshot);
    let mut violations = Vec::new();
    let mut violated = |invariant: &'static str, detail: String| {
        violations.push(InvariantViolation {
            frame: index,
            site: frame.site().to_owned(),
            invariant,
            detail,
        });
    };

    // 0. The UNCONDITIONAL check. Everything below is conditional on the
    //    classifier's own verdict and therefore goes vacuous the moment it
    //    stops returning Ready; this one does not.
    match frame.expect_ready() {
        Some(true) if state != TerminalScreenState::Ready => {
            violated(
                "a_frame_known_to_be_ready_classifies_ready",
                format!(
                    "this frame was independently established as a provably empty \
                     composer, and the classifier returned {state:?}"
                ),
            );
        }
        Some(false) if state == TerminalScreenState::Ready => {
            violated(
                "a_frame_known_not_to_be_ready_does_not_classify_ready",
                "this frame was independently established as NOT a ready composer, \
                 and the classifier returned Ready"
                    .to_owned(),
            );
        }
        _ => {}
    }

    // 1. Ready is a claim about a composer, so a Ready frame must have one.
    if state == TerminalScreenState::Ready {
        let Some(geometry) = geometry else {
            violated(
                "ready_implies_editor",
                "classified Ready with no cursor-correlated editor".to_owned(),
            );
            return violations;
        };

        // 2. The 85/85 calibration, asserted rather than remembered.
        if geometry.rendered_rows_below_cursor != MEASURED_RENDERED_ROWS_BELOW_READY_COMPOSER {
            violated(
                "ready_renders_two_rows_below_cursor",
                format!(
                    "{} rendered rows below the cursor, MEASURED {}",
                    geometry.rendered_rows_below_cursor,
                    MEASURED_RENDERED_ROWS_BELOW_READY_COMPOSER
                ),
            );
        }

        // 3. An empty composer is the cursor two cells after its own glyph, on
        //    the glyph's own row. This is the definition Ready is decided by, so
        //    a Ready frame that fails it means the classifier and this module
        //    disagree about what they are both reading.
        if geometry.cursor_row_from_anchor != 0 || geometry.cursor_col_from_prompt != 2 {
            violated(
                "ready_cursor_is_two_cells_after_the_glyph",
                format!(
                    "cursor is +{} rows +{} cols from the anchor",
                    geometry.cursor_row_from_anchor, geometry.cursor_col_from_prompt
                ),
            );
        }

        // 4. Ready without a glyph would mean readiness was inferred from
        //    something other than a composer.
        if !snapshot.visible_text.contains('\u{276f}') {
            violated(
                "ready_implies_prompt_glyph",
                "classified Ready with no composer glyph anywhere on screen".to_owned(),
            );
        }
    }

    // 5. No glyph implies no editor, on every frame and not only on Ready ones.
    if !snapshot.visible_text.contains('\u{276f}') && geometry.is_some() {
        violated(
            "no_glyph_implies_no_editor",
            "resolved an editor on a screen with no composer glyph".to_owned(),
        );
    }

    // 6. A resolved editor must sit inside the frame it claims to be at the end
    //    of. This is the composer bug's invariant, stated positively.
    if let Some(geometry) = geometry {
        if geometry.cursor_row > geometry.last_rendered_row {
            violated(
                "editor_cursor_is_inside_the_rendered_frame",
                format!(
                    "cursor row {} is below the last rendered row {}",
                    geometry.cursor_row, geometry.last_rendered_row
                ),
            );
        }
        if geometry.anchor_row > geometry.cursor_row {
            violated(
                "editor_anchor_is_at_or_above_the_cursor",
                format!(
                    "anchor row {} is below cursor row {}",
                    geometry.anchor_row, geometry.cursor_row
                ),
            );
        }
    }

    // 7. The frame's own row count must match the text it carries, or the
    //    cursor is being correlated against the wrong textual row.
    let line_count = snapshot.visible_text.split('\n').count();
    if line_count != usize::from(snapshot.rows) {
        violated(
            "row_count_matches_rendered_text",
            format!(
                "{} rows declared, {} lines rendered",
                snapshot.rows, line_count
            ),
        );
    }

    violations
}

/// Runs every invariant over every frame of a corpus.
#[must_use]
pub fn check_corpus(corpus: &Corpus) -> Vec<InvariantViolation> {
    corpus
        .frames
        .iter()
        .enumerate()
        .flat_map(|(index, frame)| check_frame(index, frame))
        .collect()
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

static RECORDER: OnceLock<Option<ScreenRecorder>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// How many frames the recorder discarded because its queue was full. Non-zero
/// means the corpus is a sample, not a transcript.
#[must_use]
pub fn dropped_frames() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

struct ScreenRecorder {
    frames: SyncSender<CorpusFrame>,
}

impl ScreenRecorder {
    /// Offers a frame. Never blocks, never allocates on the failure path, and
    /// never propagates an error: a recording fault must not become a session
    /// fault.
    fn offer(&self, frame: CorpusFrame) {
        match self.frames.try_send(frame) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// The process-wide recorder, or `None` when [`CORPUS_DIR_ENV`] is unset.
///
/// Initialized exactly once. The disabled path after that is one acquire load
/// and a `None` match, which is what keeps this off the production path's cost
/// in every build that is not recording.
fn recorder() -> Option<&'static ScreenRecorder> {
    RECORDER.get_or_init(spawn_recorder).as_ref()
}

fn spawn_recorder() -> Option<ScreenRecorder> {
    let dir = PathBuf::from(std::env::var_os(CORPUS_DIR_ENV)?);
    let label = std::env::var(CORPUS_LABEL_ENV).unwrap_or_else(|_| "session".to_owned());
    spawn_recorder_in(&dir, label).map(|(recorder, _)| recorder)
}

/// The recorder's whole mechanism, with the environment lookup lifted out.
///
/// Split from [`spawn_recorder`] so the enabled path is reachable from a test.
/// The process-wide recorder is a `OnceLock` initialized from the environment on
/// first use, so a test can neither enable it after another test has disabled it
/// nor observe it twice; without this seam the channel, the writer thread and
/// the drop-on-full behaviour are all unreachable and therefore unverified.
///
/// Returns the path it is writing to so a caller can read the recording back.
fn spawn_recorder_in(dir: &Path, label: String) -> Option<(ScreenRecorder, PathBuf)> {
    // Geometry is per frame; the stamp records 0x0 because the recorder is
    // process-wide and does not know which pane will speak first. Claiming a
    // size here would be claiming a measurement nothing took.
    let stamp = CorpusStamp::for_host(0, 0, label);
    let path = dir.join(format!(
        "pmux-screens-{}-{}.ndjson",
        stamp.recorded_unix_ms,
        std::process::id()
    ));
    let mut writer = CorpusWriter::create(&path, &stamp).ok()?;
    let (frames, receiver) = sync_channel::<CorpusFrame>(RECORDER_QUEUE_DEPTH);
    // A dedicated OS thread, not a tokio task: the frames being recorded are
    // produced by a 25 ms poll on the runtime, and a corpus write must never be
    // schedulable ahead of the poll it is observing.
    std::thread::Builder::new()
        .name("pmux-screen-corpus".to_owned())
        .spawn(move || {
            while let Ok(frame) = receiver.recv() {
                // A write fault ends the recording and nothing else. The session
                // this is observing is not the recorder's to fail.
                if writer.append(&frame).is_err() {
                    break;
                }
            }
        })
        .ok()?;
    Some((ScreenRecorder { frames }, path))
}

/// Records one plain-text snapshot, if recording is enabled.
///
/// `site` names the production read, so a replay can select the frames one gate
/// actually saw. The snapshot is borrowed and only cloned once a recorder
/// exists, which is what makes the disabled call free.
pub fn record_snapshot(site: &str, snapshot: &TerminalSnapshot) {
    if let Some(recorder) = recorder() {
        recorder.offer(CorpusFrame::from_snapshot(site, snapshot));
    }
}

/// Records one styled screen, if recording is enabled.
pub fn record_styled(site: &str, screen: &StyledScreen) {
    if let Some(recorder) = recorder() {
        recorder.offer(CorpusFrame::from_styled(site, screen));
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(site: &str) -> CorpusFrame {
        CorpusFrame::from_snapshot(
            site,
            &TerminalSnapshot {
                revision: 1,
                rows: 2,
                cols: 4,
                cursor: Some(TerminalCursor {
                    row: 0,
                    col: 2,
                    visible: true,
                    style: 0,
                }),
                visible_text: "\u{276f} \nx".to_owned(),
            },
        )
    }

    /// The ENABLED recorder, end to end: offer, cross the channel, get written by
    /// the writer thread, and reload as the same frames in the same order.
    ///
    /// The process-wide recorder is initialized once from the environment, so
    /// nothing that goes through `record_snapshot` can reach this path twice in
    /// one process. Without [`spawn_recorder_in`] the channel, the thread and the
    /// file it produces were entirely unverified -- the only recorder test was
    /// that the DISABLED path does nothing, which every possible implementation
    /// of a broken recorder also satisfies.
    #[test]
    fn an_enabled_recorder_writes_every_offered_frame_in_order() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let (recorder, path) =
            spawn_recorder_in(directory.path(), "unit test".to_owned()).expect("a recorder");

        for index in 0..64 {
            recorder.offer(frame(&format!("site-{index}")));
        }
        // Dropping the sender disconnects the channel, which is what ends the
        // writer thread's `recv` loop and flushes the file.
        drop(recorder);

        // The writer runs on its own thread, so the file is complete only once
        // that thread has drained and exited. Poll rather than sleep a fixed
        // interval: a fixed sleep is a flake on a loaded host.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let corpus = loop {
            if let Ok(corpus) = Corpus::load(&path)
                && corpus.frames.len() == 64
            {
                break corpus;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the recorder did not write 64 frames within 10s"
            );
            std::thread::yield_now();
        };

        for (index, frame) in corpus.frames.iter().enumerate() {
            assert_eq!(
                frame.site(),
                format!("site-{index}"),
                "frames were reordered or dropped in the channel"
            );
        }
        assert_eq!(
            corpus.stamp.schema, CORPUS_SCHEMA,
            "a recording must stamp its own schema"
        );
    }

    /// A full queue drops frames and never blocks.
    ///
    /// This is the property that keeps recording off the 25 ms poll's critical
    /// path, and it is the one that would be silently lost by swapping
    /// `try_send` for `send`. Offering far more than the queue depth without a
    /// reader running must still return, and the loss must be counted rather
    /// than hidden.
    #[test]
    fn a_full_queue_drops_frames_instead_of_blocking_the_poll() {
        let (frames, receiver) = sync_channel::<CorpusFrame>(4);
        let recorder = ScreenRecorder { frames };
        let before = dropped_frames();

        // No reader is draining `receiver`, so everything past the queue depth
        // has nowhere to go. If `offer` blocked, this test would hang.
        for index in 0..1_000 {
            recorder.offer(frame(&format!("site-{index}")));
        }

        assert!(
            dropped_frames() > before,
            "a full queue must count the frames it discarded"
        );
        drop(receiver);
    }
}
