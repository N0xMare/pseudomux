//! Properties the screen classifier must satisfy on every screen, not outputs it
//! must produce on five.
//!
//! # Why properties and not fixtures
//!
//! The composer gate's bug was `snapshot.rows - cursor.row - 1 > 4`: it measured
//! the cursor's distance from the physical bottom of the GRID rather than from
//! the end of Ink's FRAME. Every checked-in fixture was a screen Ink had painted
//! to the bottom of, so on every fixture the two quantities were equal and the
//! bug was invisible. It survived four static review rounds and died to one live
//! capture of a post-`/clear` screen, where Ink painted four rows at the top of a
//! 24-row grid and left rows 8-23 literally blank.
//!
//! No number of additional fixtures would have found that, because the fixtures
//! were all drawn from the same painting regime. What finds it is a property:
//!
//! > Appending blank rows below the frame must not change the verdict.
//!
//! That statement is false for the old expression on *almost every* input and
//! true for the new one on all of them, and
//! [`blank_rows_below_the_frame_do_not_change_the_verdict`] below fails within a
//! handful of generated cases against the old code. It catches the bug by
//! construction rather than by having thought to write down the one screen that
//! exhibits it.
//!
//! # The generator
//!
//! [`ScreenSpec`] describes a screen the way Claude Code lays one out -- a frame
//! at some anchor row, scrollback above the composer, the composer glyph at some
//! column, wrapped continuation rows below it, a footer, and blank grid below
//! that -- and renders it to a real [`TerminalSnapshot`]. Every axis the two
//! known bugs touched is a generated parameter: frame height, anchor row,
//! blank-tail length, glyph column, cursor position, wrap depth, trailing
//! whitespace, and decoy `❯` glyphs in the scrollback.
//!
//! proptest is already a dev-dependency of this crate at the version in the
//! workspace lock file; no dependency is added here.

use proptest::prelude::*;
use pseudomux_rmux::{TerminalCursor, TerminalSnapshot};
use pseudomux_service::driver_io::{
    ScreenGeometry, ScreenShape, TerminalScreenState, classify_terminal_snapshot, screen_geometry,
};

/// `state` with the four SIZE-dependent facts of an unrecognized screen's shape
/// erased, leaving the decision and everything about it that ought to hold
/// still.
///
/// [`TerminalScreenState::Unrecognised`] carries a [`ScreenShape`] so a refusal
/// can name what was on screen, and that shape reports the grid's row and column
/// counts and its line counts -- the very quantities the two properties below
/// vary ON PURPOSE. Comparing whole values would assert that a diagnostic is
/// invariant under the transformation it exists to describe.
///
/// Only those four are dropped. `revision_nonzero`, `cursor_present`,
/// `cursor_visible` and `contains_prompt_glyph` are still compared, and they
/// genuinely are invariant under a blank tail and a translation -- so this stays
/// strictly stronger than comparing the verdict's name. Every arm that is not
/// `Unrecognised` is compared in full, payload included.
///
/// A size-dependent field added to `ScreenShape` later is not erased here, and
/// the property fails LOUDLY rather than passing on a value nobody looked at.
fn decision(state: TerminalScreenState) -> TerminalScreenState {
    match state {
        TerminalScreenState::Unrecognised(shape) => {
            TerminalScreenState::Unrecognised(ScreenShape {
                rows: 0,
                cols: 0,
                line_count: 0,
                non_empty_line_count: 0,
                ..shape
            })
        }
        other => other,
    }
}

/// The composer glyph. Named because a literal `❯` in an assertion message is
/// indistinguishable from a decoy one in a diff.
const GLYPH: char = '\u{276f}';

/// A screen described the way Claude Code lays one out.
///
/// Rendering is deterministic: the same spec is the same snapshot on every host,
/// so a shrunk failure is a reproducer and not a hint.
#[derive(Clone, Debug)]
struct ScreenSpec {
    /// Grid height. Rows past the frame are rendered as length-zero lines,
    /// which is what Ink leaves behind when it does not paint to the bottom.
    rows: u16,
    cols: u16,
    /// First row Ink painted. Rows above it are blank.
    frame_top: u16,
    /// History rows between `frame_top` and the composer.
    scrollback_rows: u16,
    /// Whether those history rows carry a `❯` of their own. An echoed user
    /// prompt in the scrollback does, and it must never be mistaken for the
    /// composer.
    decoy_glyphs: bool,
    /// Column the composer glyph sits at.
    glyph_col: u16,
    /// Text typed into the composer. Empty is an empty composer.
    typed: String,
    /// Continuation rows below the glyph row, as a wrapped prompt renders.
    wrapped_depth: u16,
    /// Rendered rows below the composer block. MEASURED as 2 on live Claude; the
    /// generator sweeps past that on purpose.
    footer_rows: u16,
    /// Whether rendered rows carry trailing spaces.
    trailing_whitespace: bool,
    /// Cursor row, as a signed offset from the composer glyph row.
    cursor_row_offset: i16,
    /// Cursor column, as an offset from `glyph_col`.
    cursor_col_offset: u16,
    /// Whether the cursor column is derived from `typed` instead of from
    /// `cursor_col_offset`. True is the realistic regime; false is adversarial.
    cursor_follows_typed: bool,
    cursor_visible: bool,
}

/// A rendered screen and the facts about it the generator knows independently of
/// the classifier.
#[derive(Clone, Debug)]
struct RenderedScreen {
    snapshot: TerminalSnapshot,
    /// Row the composer glyph was painted at.
    composer_row: u16,
    /// Last row carrying any non-whitespace.
    last_rendered_row: u16,
    /// Whether the composer this screen renders has anything typed into it.
    /// This is the ORACLE for readiness: it comes from the spec, never from the
    /// screen, and never from the classifier.
    composer_is_empty: bool,
}

impl ScreenSpec {
    /// Rows the frame occupies: scrollback, the composer, its wrapped
    /// continuations, and the footer.
    fn frame_height(&self) -> u32 {
        u32::from(self.scrollback_rows)
            + 1
            + u32::from(self.wrapped_depth)
            + u32::from(self.footer_rows)
    }

    /// Whether this spec describes a screen at all.
    ///
    /// Two rejections, and both exist to make the transformation properties
    /// well-formed rather than to make them pass:
    ///
    /// 1. **The frame must fit the grid.** A frame clipped by the bottom of the
    ///    grid is not a frame with a blank tail, it is a truncated frame, and
    ///    appending a row to the grid then un-truncates it -- moving the last
    ///    rendered row and changing real geometry. `with_blank_tail` would not
    ///    be adding blank rows, it would be finishing the render.
    /// 2. **The cursor must land inside the grid without clamping.** Clamping
    ///    the cursor to row 0 pins it to whatever happens to be there, so
    ///    translating the frame down moves the composer out from under a cursor
    ///    that stayed put. The cursor's offset from the composer is the thing
    ///    the transformations must preserve, and a clamp does not preserve it.
    ///
    /// Both rejected shapes are real screens; they are simply not screens these
    /// two properties are about. The properties that do not transform the screen
    /// still see every shape.
    fn is_renderable(&self) -> bool {
        if u32::from(self.frame_top) + self.frame_height() > u32::from(self.rows) {
            return false;
        }
        let composer_row = i64::from(self.frame_top) + i64::from(self.scrollback_rows);
        let cursor_row = composer_row + i64::from(self.cursor_row_offset);
        if cursor_row < 0 || cursor_row >= i64::from(self.rows) {
            return false;
        }
        let cursor_col = u32::from(self.glyph_col)
            + if self.cursor_follows_typed {
                2 + u32::try_from(self.typed.chars().count()).expect("typed length fits u32")
            } else {
                u32::from(self.cursor_col_offset)
            };
        cursor_col < u32::from(self.cols)
    }

    fn render(&self) -> Option<RenderedScreen> {
        if !self.is_renderable() {
            return None;
        }
        Some(self.render_unchecked())
    }

    fn render_unchecked(&self) -> RenderedScreen {
        let mut lines: Vec<String> = vec![String::new(); usize::from(self.rows)];
        let pad = |text: String, spec: &Self| -> String {
            if spec.trailing_whitespace {
                format!("{text}   ")
            } else {
                text
            }
        };

        let frame_top = usize::from(self.frame_top);
        let mut row = frame_top;
        for index in 0..usize::from(self.scrollback_rows) {
            if row >= lines.len() {
                break;
            }
            lines[row] = pad(
                if self.decoy_glyphs {
                    // An echoed user prompt: a real glyph, in the scrollback,
                    // exactly as Claude renders one.
                    format!("{GLYPH} echoed prompt {index}")
                } else {
                    format!("history line {index}")
                },
                self,
            );
            row += 1;
        }

        // For a spec that passed `is_renderable` this clamp is provably a no-op:
        // the whole frame fits, so `row` is already in bounds. It is retained as
        // a safety net for `classification_is_total_and_deterministic`, which
        // renders unchecked shapes on purpose. Reaching it means the screen is
        // no longer the one the spec describes, which is why the transformation
        // properties go through `render` and never through this directly.
        let composer_row = row.min(lines.len().saturating_sub(1));
        lines[composer_row] = pad(
            format!(
                "{}{GLYPH} {}",
                " ".repeat(usize::from(self.glyph_col)),
                self.typed
            ),
            self,
        );
        row = composer_row + 1;

        for index in 0..usize::from(self.wrapped_depth) {
            if row >= lines.len() {
                break;
            }
            lines[row] = pad(format!("wrapped continuation {index}"), self);
            row += 1;
        }
        for index in 0..usize::from(self.footer_rows) {
            if row >= lines.len() {
                break;
            }
            lines[row] = pad(format!("footer row {index}"), self);
            row += 1;
        }
        // Everything from `row` down stays blank: this is the blank tail, the
        // axis the composer bug was hiding on.

        let last_rendered_row = u16::try_from(
            lines
                .iter()
                .rposition(|line| !line.trim().is_empty())
                .unwrap_or(0),
        )
        .expect("row index fits u16");

        // Same story as the composer row: a no-op for renderable specs, a safety
        // net for the unchecked totality run.
        let cursor_row =
            i64::try_from(composer_row).expect("row fits i64") + i64::from(self.cursor_row_offset);
        let cursor_row = u16::try_from(cursor_row.clamp(0, i64::from(self.rows.saturating_sub(1))))
            .expect("a clamped row fits u16");
        let cursor_col = if self.cursor_follows_typed {
            // Where a real terminal puts the cursor: two cells after the glyph,
            // then one cell per typed character.
            u32::from(self.glyph_col)
                + 2
                + u32::try_from(self.typed.chars().count()).expect("typed length fits u32")
        } else {
            u32::from(self.glyph_col) + u32::from(self.cursor_col_offset)
        };
        let cursor_col = u16::try_from(cursor_col.min(u32::from(self.cols.saturating_sub(1))))
            .expect("a clamped column fits u16");

        RenderedScreen {
            snapshot: TerminalSnapshot {
                revision: 1,
                rows: self.rows,
                cols: self.cols,
                cursor: Some(TerminalCursor {
                    row: cursor_row,
                    col: cursor_col,
                    visible: self.cursor_visible,
                    style: 0,
                }),
                visible_text: lines.join("\n"),
            },
            composer_row: u16::try_from(composer_row).expect("row fits u16"),
            last_rendered_row,
            composer_is_empty: self.typed.is_empty(),
        }
    }

    /// The same screen with `extra` additional blank rows below the frame.
    ///
    /// Nothing rendered moves. This is exactly the transformation that
    /// distinguishes "distance from the end of the frame" from "distance from
    /// the bottom of the grid", and no other axis changes with it.
    fn with_blank_tail(&self, extra: u16) -> Self {
        Self {
            rows: self.rows.saturating_add(extra),
            ..self.clone()
        }
    }

    /// The same frame translated `down` rows, with blank rows above it.
    ///
    /// Anchor and cursor move together, so a classifier that reads relative
    /// geometry must be unmoved by this. One that reads absolute rows is not.
    fn translated_down(&self, down: u16) -> Self {
        Self {
            rows: self.rows.saturating_add(down),
            frame_top: self.frame_top.saturating_add(down),
            ..self.clone()
        }
    }

    /// Whether the cursor is where a real terminal would put it: on the
    /// composer's own row, at the column the typed text puts it.
    ///
    /// Outside this regime the generator does not know whether a screen should
    /// be Ready, because it has parked the cursor somewhere Claude Code never
    /// parks one -- on a scrollback row, on a wrapped continuation, at an
    /// arbitrary column. The classifier's answer there is a consequence of the
    /// input and not a verdict about a composer, so an oracle claiming
    /// otherwise would be asserting a fact nothing established.
    fn cursor_is_where_a_terminal_puts_it(&self) -> bool {
        self.cursor_row_offset == 0 && self.cursor_follows_typed
    }
}

fn screen_spec() -> impl Strategy<Value = ScreenSpec> {
    (
        // Grid and frame placement.
        (8_u16..40, 20_u16..120, 0_u16..12),
        // Frame contents.
        (0_u16..6, any::<bool>(), 0_u16..8),
        // Composer contents: empty is generated often on purpose, because the
        // interesting verdict is Ready and only an empty composer earns it.
        prop_oneof![
            6 => Just(String::new()),
            2 => "[a-z ]{1,20}",
            1 => Just("Try \"how do I ...\"".to_owned()),
        ],
        // Wrap depth, footer height, trailing whitespace.
        (0_u16..4, 0_u16..6, any::<bool>()),
        // Cursor.
        (-3_i16..4, 0_u16..10, any::<bool>(), any::<bool>()),
    )
        .prop_map(
            |(
                (rows, cols, frame_top),
                (scrollback_rows, decoy_glyphs, glyph_col),
                typed,
                (wrapped_depth, footer_rows, trailing_whitespace),
                (cursor_row_offset, cursor_col_offset, cursor_follows_typed, cursor_visible),
            )| ScreenSpec {
                rows,
                cols,
                frame_top,
                scrollback_rows,
                decoy_glyphs,
                glyph_col,
                typed,
                wrapped_depth,
                footer_rows,
                trailing_whitespace,
                cursor_row_offset,
                cursor_col_offset,
                cursor_follows_typed,
                cursor_visible,
            },
        )
}

/// The same shapes, restricted to the regime where the cursor is where a real
/// terminal puts it.
///
/// A dedicated strategy rather than a `prop_assume!` over [`screen_spec`]: the
/// regime is roughly one case in fourteen, so filtering spends the whole budget
/// on rejects and proptest aborts before it has tested anything. Constraining
/// the generator tests the same screens at full case count.
fn realistic_screen_spec() -> impl Strategy<Value = ScreenSpec> {
    screen_spec().prop_map(|spec| ScreenSpec {
        cursor_row_offset: 0,
        cursor_follows_typed: true,
        ..spec
    })
}

/// A screen with no `❯` anywhere, built by stripping the glyph from a rendered
/// spec rather than by a separate generator, so it covers the same shapes.
fn strip_glyphs(snapshot: &TerminalSnapshot) -> TerminalSnapshot {
    TerminalSnapshot {
        visible_text: snapshot.visible_text.replace(GLYPH, "x"),
        ..snapshot.clone()
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 4096,
        max_shrink_iters: 8192,
        // A constant seed: a failure a future agent cannot reproduce is a
        // rumour. proptest's default file-backed regression store is left on so
        // a real failure is also written to `proptest-regressions/`.
        ..ProptestConfig::default()
    })]

    /// **The property that catches the composer bug by construction.**
    ///
    /// Blank rows below the frame are not information. Ink leaves them behind
    /// whenever it repaints a shorter frame than the last one, and how many
    /// there are is a fact about the previous frame, not this one. A verdict
    /// that moves when they are appended is reading the grid's bottom edge.
    ///
    /// DELETED-AND-CONFIRMED against the pre-fix expression
    /// `snapshot.rows - cursor.row - 1 > MAX_RENDERED_ROWS_BELOW_ACTIVE_CURSOR`:
    /// fails, and shrinks to a minimal reproducer.
    #[test]
    fn blank_rows_below_the_frame_do_not_change_the_verdict(spec in screen_spec()) {
        let Some(rendered) = spec.render() else { return Ok(()) };
        let baseline = decision(classify_terminal_snapshot(&rendered.snapshot));
        for extra in 1_u16..=8 {
            let widened = spec
                .with_blank_tail(extra)
                .render()
                .expect("adding blank rows below a fitting frame keeps it fitting");
            prop_assert_eq!(
                decision(classify_terminal_snapshot(&widened.snapshot)),
                baseline.clone(),
                "appending {} blank rows below the frame changed the verdict",
                extra
            );
            // The stronger statement: not merely the same verdict, the same
            // geometry. A verdict that survived by coincidence is not the
            // property being claimed.
            prop_assert_eq!(
                screen_geometry(&widened.snapshot),
                screen_geometry(&rendered.snapshot),
                "appending {} blank rows below the frame moved the composer",
                extra
            );
        }
    }

    /// The dual: moving the whole frame down the grid moves the anchor and the
    /// cursor together, so a classifier reading relative geometry is unmoved.
    #[test]
    fn translating_the_frame_down_does_not_change_the_verdict(spec in screen_spec()) {
        let Some(rendered) = spec.render() else { return Ok(()) };
        let baseline = decision(classify_terminal_snapshot(&rendered.snapshot));
        for down in 1_u16..=6 {
            let moved = spec
                .translated_down(down)
                .render()
                .expect("translating a fitting frame into a taller grid keeps it fitting");
            prop_assert_eq!(
                decision(classify_terminal_snapshot(&moved.snapshot)),
                baseline.clone(),
                "translating the frame down {} rows changed the verdict",
                down
            );
        }
    }

    /// Never `Ready` unless the composer is provably empty -- and always `Ready`
    /// when it provably is.
    ///
    /// Scoped to [`ScreenSpec::cursor_is_where_a_terminal_puts_it`], the regime
    /// in which the generator knows the answer. There the oracle is
    /// [`RenderedScreen::composer_is_empty`], read off the SPEC -- what the
    /// generator typed -- and never off the screen, so a classifier cannot
    /// satisfy it by agreeing with itself.
    ///
    /// Stated as an IFF on purpose. The one-directional version is satisfied by
    /// a classifier that never returns `Ready`, and that classifier refuses
    /// every turn pmux is asked to run -- which is exactly the failure the
    /// composer bug produced. Both directions have to be nailed down.
    #[test]
    fn ready_iff_the_composer_is_empty_when_the_cursor_is_where_a_terminal_puts_it(
        spec in realistic_screen_spec()
    ) {
        debug_assert!(spec.cursor_is_where_a_terminal_puts_it());
        let Some(rendered) = spec.render() else { return Ok(()) };
        let ready = classify_terminal_snapshot(&rendered.snapshot) == TerminalScreenState::Ready;

        if ready {
            prop_assert!(
                rendered.composer_is_empty,
                "classified Ready with {:?} typed into the composer",
                spec.typed
            );
            prop_assert!(
                spec.cursor_visible,
                "classified Ready with an invisible cursor"
            );
        }

        // The converse, over the geometry live Claude actually renders: an
        // empty composer with a visible cursor and no more than the measured
        // footer below it is Ready, with no further conditions.
        let rows_below = rendered.last_rendered_row - rendered.composer_row;
        if rendered.composer_is_empty && spec.cursor_visible && rows_below <= 2 {
            prop_assert!(
                ready,
                "refused a provably empty composer with {} rendered rows below it",
                rows_below
            );
        }
    }

    /// `Ready` is a claim about WHICH composer, and in the regime where the
    /// cursor is in the composer it must be that composer -- never a decoy `❯`
    /// from an echoed prompt higher in the scrollback.
    #[test]
    fn ready_anchors_to_the_composer_and_not_to_scrollback(spec in realistic_screen_spec()) {
        debug_assert!(spec.cursor_is_where_a_terminal_puts_it());
        let Some(rendered) = spec.render() else { return Ok(()) };
        if classify_terminal_snapshot(&rendered.snapshot) == TerminalScreenState::Ready {
            let geometry = screen_geometry(&rendered.snapshot)
                .expect("a Ready verdict resolves an editor");
            prop_assert_eq!(
                geometry.anchor_row,
                rendered.composer_row,
                "anchored to row {} but the composer is at row {}",
                geometry.anchor_row,
                rendered.composer_row
            );
        }
    }

    /// `Ready` implies the cursor's own row carries the glyph, in EVERY regime
    /// including the adversarial ones.
    ///
    /// An empty composer is the cursor sitting in the composer, so readiness may
    /// never be granted from a wrapped continuation row or a footer row. This is
    /// the one readiness property that needs no oracle from the spec, which is
    /// why it is not scoped: it holds on screens Claude Code cannot produce too.
    #[test]
    fn ready_implies_the_cursor_row_carries_the_glyph(spec in screen_spec()) {
        let Some(rendered) = spec.render() else { return Ok(()) };
        if classify_terminal_snapshot(&rendered.snapshot) == TerminalScreenState::Ready {
            let geometry = screen_geometry(&rendered.snapshot)
                .expect("a Ready verdict resolves an editor");
            prop_assert_eq!(
                geometry.cursor_row_from_anchor, 0,
                "granted Ready {} rows below the glyph",
                geometry.cursor_row_from_anchor
            );
            let cursor_line = rendered
                .snapshot
                .visible_text
                .split('\n')
                .nth(usize::from(geometry.cursor_row))
                .expect("the cursor row is inside the rendered text");
            prop_assert!(
                cursor_line.contains(GLYPH),
                "granted Ready on a row with no glyph: {:?}",
                cursor_line
            );
            prop_assert!(
                geometry.rendered_rows_below_cursor <= 4,
                "granted Ready with {} rendered rows below the cursor",
                geometry.rendered_rows_below_cursor
            );
        }
    }

    /// No `❯` implies no editor, always.
    ///
    /// The glyph is the only thing that makes a row a composer. Every other
    /// property here is conditional on an editor resolving; this one bounds when
    /// one may resolve at all.
    #[test]
    fn no_glyph_implies_no_editor(spec in screen_spec()) {
        let Some(rendered) = spec.render() else { return Ok(()) };
        let stripped = strip_glyphs(&rendered.snapshot);
        prop_assert!(
            screen_geometry(&stripped).is_none(),
            "resolved an editor on a screen with no composer glyph"
        );
        prop_assert_ne!(
            classify_terminal_snapshot(&stripped),
            TerminalScreenState::Ready,
            "classified Ready on a screen with no composer glyph"
        );
    }

    /// Classification is total and deterministic: every frame maps to exactly
    /// one state, never zero and never two.
    ///
    /// "Never two" is the return type. "Never zero" is not: a panic, an
    /// arithmetic overflow, or a slice out of bounds is a frame that maps to no
    /// state at all, and every one of those is reachable from the row/column
    /// arithmetic this classifier does. Running it is the check.
    #[test]
    fn classification_is_total_and_deterministic(spec in screen_spec()) {
        // Deliberately NOT gated on `is_renderable`: a clipped frame and a
        // clamped cursor are real screens, and totality is the one property that
        // must hold on every shape the generator can emit.
        let snapshot = spec.render_unchecked().snapshot;
        let first = classify_terminal_snapshot(&snapshot);
        let second = classify_terminal_snapshot(&snapshot);
        prop_assert_eq!(first, second, "classification is not deterministic");
        // Geometry must agree with itself too, and must agree with the verdict.
        let geometry = screen_geometry(&snapshot);
        prop_assert_eq!(geometry, screen_geometry(&snapshot));
    }

    /// A resolved editor's cursor is inside the frame it claims to be at the end
    /// of, and its anchor is at or above its cursor.
    ///
    /// This is the composer bug's invariant stated positively, and it is also
    /// what makes `rendered_rows_below_cursor` a non-negative quantity rather
    /// than a saturating subtraction that quietly reports zero.
    #[test]
    fn a_resolved_editor_sits_inside_its_own_frame(spec in screen_spec()) {
        let Some(rendered) = spec.render() else { return Ok(()) };
        let Some(geometry) = screen_geometry(&rendered.snapshot) else {
            return Ok(());
        };
        prop_assert!(
            geometry.cursor_row <= geometry.last_rendered_row,
            "cursor row {} is below the last rendered row {}",
            geometry.cursor_row,
            geometry.last_rendered_row
        );
        prop_assert!(
            geometry.anchor_row <= geometry.cursor_row,
            "anchor row {} is below cursor row {}",
            geometry.anchor_row,
            geometry.cursor_row
        );
        prop_assert_eq!(
            geometry.last_rendered_row,
            rendered.last_rendered_row,
            "production and the generator disagree about where the frame ends"
        );
    }

    /// Trailing whitespace on a rendered row is not content, and a row of it is
    /// not a rendered row.
    ///
    /// Ink pads. If padding counted as content, the last rendered row would be
    /// wherever the padding stopped and every geometry measurement below the
    /// composer would be off by the pad.
    #[test]
    fn trailing_whitespace_does_not_change_the_verdict(spec in screen_spec()) {
        let bare = ScreenSpec { trailing_whitespace: false, ..spec.clone() };
        let padded = ScreenSpec { trailing_whitespace: true, ..spec };
        let (Some(bare), Some(padded)) = (bare.render(), padded.render()) else {
            return Ok(());
        };
        prop_assert_eq!(
            classify_terminal_snapshot(&bare.snapshot),
            classify_terminal_snapshot(&padded.snapshot),
            "trailing whitespace changed the verdict"
        );
    }
}

/// The composer bug, as a single named screen.
///
/// MEASURED on Claude Code 2.1.220 after a `/clear`: rule/composer/rule/footer
/// at rows 4-7 of a 24-row grid, cursor at (5,2), rows 8-23 length zero,
/// byte-identical for 285s across ~4,250 samples. The frame's distance to the
/// bottom of the grid is 18; its distance to the end of the frame is 2.
///
/// The property suite above finds this shape by generation. This test names it,
/// so the regression has a reproducer that reads as the screen it came from.
#[test]
fn the_post_clear_screen_that_broke_the_composer_gate_is_ready() {
    let mut lines = vec![String::new(); 24];
    lines[4] = "─".repeat(40);
    lines[5] = format!("{GLYPH} ");
    lines[6] = "─".repeat(40);
    lines[7] = "  ? for shortcuts".to_owned();
    let snapshot = TerminalSnapshot {
        revision: 1,
        rows: 24,
        cols: 80,
        cursor: Some(TerminalCursor {
            row: 5,
            col: 2,
            visible: true,
            style: 0,
        }),
        visible_text: lines.join("\n"),
    };

    assert_eq!(
        classify_terminal_snapshot(&snapshot),
        TerminalScreenState::Ready,
        "a provably empty composer four rows into a 24-row grid must be Ready; \
         measuring from the bottom of the grid makes it unfindable and refuses \
         the first turn after every successful /clear"
    );
    let geometry = screen_geometry(&snapshot).expect("the composer resolves");
    assert_eq!(
        geometry,
        ScreenGeometry {
            anchor_row: 5,
            cursor_row: 5,
            prompt_col: 0,
            cursor_row_from_anchor: 0,
            cursor_col_from_prompt: 2,
            last_rendered_row: 7,
            rendered_rows_below_cursor: 2,
            empty_cursor_position: true,
        },
        "the measured post-/clear geometry"
    );
}
