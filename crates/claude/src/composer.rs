//! The one rule about what a Claude Code composer will record verbatim.
//!
//! # Why this is a crate-level module and not a guard in the daemon
//!
//! A pmux turn is proven by equality. [`crate::normalize_prompt`] is applied to
//! the text pmux typed and to the text Claude recorded, and
//! `TranscriptEngine::ingest` refuses the turn with
//! [`crate::TranscriptError::UnexpectedTypedPrompt`] when the two differ. So a
//! prompt pmux admits and the composer does not record verbatim is not a
//! cosmetic problem: it is a turn that can never be acknowledged, and on Path B
//! it is a pooled instance destroyed for a caller input pmux said was legal.
//!
//! Every entry point that types a prompt therefore has to ask the same
//! question, and before this module they each answered it from their own copy:
//! `crates/service/src/driver_io.rs` and `bin/pmux/src/cli.rs` each carried a
//! `starts_with('/')` test and a hand-typed 24-range table of ignorable
//! prefixes, and `bin/pmux-mcp/src/tools.rs` described the rule to callers in a
//! third sentence. Two guards over one boundary fail by drifting apart. This is
//! the one they now share.
//!
//! # What was MEASURED, and how
//!
//! Claude Code 2.1.226 on macOS 15.7.7 / aarch64, driven through the shipped
//! `pmuxd` release binary as a Path B pool of one, real turns, reading the
//! child's OWN transcript rows back rather than the rendered screen
//! (`docs/path-b-adversarial.md` sec. 4.4). Every ASCII punctuation character was
//! sent as the first character of an otherwise ordinary prompt and the recorded
//! `user` row compared to the bytes pmux sent:
//!
//! ```text
//! # $ % > ? \ | ~ ^ & * - + . , : ; = ` " ' ( [ { <    recorded verbatim, turn answered
//! !                                                    NOT recorded: bash mode
//! /                                                    NOT recorded: command menu
//! ```
//!
//! `!` is the finding this module exists for. The composer reads a leading `!`
//! as a mode switch into bash mode, drops it from the buffer, and Enter then
//! runs the REST OF THE PROMPT AS A SHELL COMMAND on the host — outside the
//! tool surface, outside `--disallowedTools "*"`, outside `--permission-mode`,
//! and outside everything a Path B cell's isolation is built from. It was
//! reproduced six times out of six on a warm (post-`/clear`) instance, three of
//! them concurrently at the 15-instance cap, each writing a file that was there
//! afterwards. The turn then never acknowledges, so it also runs to the
//! caller's deadline — 600 000 ms under daemon policy.
//!
//! `\t` is the second measurement: the composer records U+0009 as four U+0020,
//! so a prompt containing a tab is admitted, typed, and then refused by the
//! acknowledgement it can never satisfy. MEASURED both mid-line (`A\tB` ->
//! `A····B`) and at the start of a line. U+000B and U+000C are the same
//! measurement taken two versions later, and they are rewrites of a different
//! shape: each is recorded as the two ASCII characters of its caret notation,
//! `^K` and `^L` (2.1.227). All three are [`COMPOSER_REWRITTEN_CHARACTERS`].
//!
//! # The direction every rule here fails in
//!
//! Refusing. A refusal costs the caller one 19 ms error naming what is wrong
//! and what would be right, spends no instance and types nothing; admitting one
//! of these costs a destroyed instance at best and a shell command at worst.
//! The two are not comparable.
//!
//! # What this does NOT claim
//!
//! That the two sets are complete for a Claude Code pmux has not measured. A new
//! mode character in a future release arrives here as a hung turn, not as a
//! wrong answer, because the acknowledgement still refuses what the composer did
//! not record.
//!
//! They ARE now complete over ASCII punctuation, which this module twice failed
//! to be able to say. The sweep first claimed all 32 and held 27; the five it
//! had never sent were then declared with a reason each, and the reason for `@`
//! was an argument rather than a measurement. **All five have now been sent** --
//! `@ ) ] } _`, each as the first character of an ordinary nonce-arithmetic
//! prompt on a warm pooled instance, each recorded verbatim and answered -- so
//! `MEASURED_FIRST_CHARACTER_SWEEP` is 32 of 32 and
//! `PUNCTUATION_THE_SWEEP_DID_NOT_SEND` is empty. The table is kept at zero
//! entries rather than deleted, because
//! `the_sweep_accounts_for_every_ascii_punctuation_character` still derives the
//! alphabet from `is_ascii_punctuation` and still refuses any character in
//! neither table, and a future character that cannot be sent needs somewhere to
//! be declared instead of somewhere to be implied away.
//!
//! # The second rule: what the composer SHOWS, which is not always what it holds
//!
//! [`composer_render_proof`] is about the other end of the same boundary. The
//! rules above decide what pmux may type; this one decides what the screen is
//! allowed to say afterwards, and it exists because the pre-Enter render gate in
//! `crates/service/src/driver_io.rs` compared geometry alone and never once
//! compared the composer's text to the prompt.
//!
//! MEASURED at Claude Code 2.1.226, macOS 15.7.7 / aarch64, a 24x120 pane,
//! through the shipped `pmuxd` release binary as a Path B pool of one, reading
//! the frames the input gate itself recorded (`PMUX_SCREEN_CORPUS_DIR`, site
//! `input_gate.post_paste`) rather than a screen scraped beside it. Eleven real
//! turns on 2026-08-09, and a twelfth ask the gate refused before Enter;
//! eighteen more on 2026-08-10, taken to settle how the composer WRAPS. The
//! first rendered row is `❯`, U+00A0, then:
//!
//! ```text
//! prompt                              rendered rows
//! 49 chars, one line                  the prompt, verbatim, one row
//! 274 chars, one line                 3 rows: 114, 116, 42 columns of text
//! 600 chars, one line                 6 rows: 115, 116, 108, 116, 111, 29
//! 229 chars, one unbroken 200-char word   2 rows: 116, 113 — a break inside the word
//! 3 lines                             the first line, continuation rows indented by 2
//! 4 lines                             [Pasted text #6 +3 lines], one row
//! 5 / 8 / 12 / 20 / 41 lines          [Pasted text #k +4/+7/+11/+19/+40 lines]
//! 1000 / 1600 / 2400 / 3021 chars     [Pasted text #k], one row
//! CJK and emoji, one line             the prompt, verbatim, no padding cell in the row text
//! CJK, 89 chars, one line             2 rows: 58 and 34 characters, 116 and 66 columns
//! ```
//!
//! **The content region is 116 columns on a 120-column pane**, established
//! three independent ways in that table: the 200-`x` word breaks at exactly
//! 116, the CJK line breaks at 58 double-width characters, and the wrapping
//! prompt's second row is filled to exactly 116. Two columns of gutter on the
//! left and two on the right, and the `cols - 2` this file used to claim was
//! two columns wide.
//!
//! **No wrap model survives that table, and nothing here encodes one.** The
//! 600-character render ends its third row 8 columns short of a width its
//! neighbours reach, with a 7-character word next — so the composer is not a
//! greedy word-wrapper at a constant width, and every "the row must be full"
//! rule refuses a render Claude actually produced. [`composer_render_proof`]
//! never asks how wide a row is; it asks whether the rows, in order, spell the
//! prompt.
//!
//! Four facts come out of that table and all four are load-bearing:
//!
//! * The cursor sits at the END of the buffer on every one of the twelve
//!   renders, so the rows from the `❯` anchor through the cursor row are the
//!   WHOLE composer and not a window on it. That is what makes a full
//!   comparison possible at all.
//! * A break consumes the character it lands on when that character is
//!   whitespace, and consumes nothing when it lands inside a word.
//!
//! * `+n` is the prompt's LINE BREAK count, exactly — 41 lines showed `+40`.
//!   It is derived from the prompt here rather than matched loosely.
//! * The ` +n lines` clause is ABSENT when the paste has no line breaks, which
//!   is why the placeholder has two forms rather than one with `+0`.
//! * `#k` is a per-process counter over COLLAPSED pastes. It ran 1,2,3,4,5,6,7
//!   across the seven of them on one pooled instance, was not advanced by the
//!   pastes rendered literally in between, and did NOT reset across the
//!   `/clear`s separating the turns — so it is not a function of the prompt and
//!   nothing here pretends to predict it. A second daemon, a fresh process,
//!   started again at `#1`.
//!
//! Collapse was observed at 4 lines and not at 3, and at 1000 single-line
//! characters and not at 600. Neither threshold is encoded: a guard that
//! refused a prompt because pmux guessed the trigger wrong would be the same
//! defect as a guard that admitted one. The band matters for one reason only —
//! a prompt too tall for the pane and too short to collapse would lose its `❯`
//! anchor and be refused — and the two measured points bracket it at six rows
//! rendered in full against a pane that can show twenty.
//!
//! # The third rule: what ENTER does, which is not always "submit this buffer"
//!
//! The two rules above are about the first character and about the screen. This
//! one is about the keystroke, and it is the one that was missing: pmux pastes a
//! prompt and then presses Enter, and Enter was assumed to submit whatever the
//! composer was holding. MEASURED at 2.1.226 -- through the shipped `pmuxd` as a
//! Path B pool of one, reading the child's own rows back, and confirmed on the
//! rendered screen of an isolated `claude` in a 120x24 pane
//! (`docs/path-b-adversarial.md` sec. 11) -- it does not:
//!
//! ```text
//! buffer                              Enter
//! "…answer as V9-<number>.   "        SUBMITS, and the recorded row has no trailing spaces
//! "…answer as VB-<number>.\n"         SUBMITS, and the recorded row has no trailing newline
//! "…answer as VC-<number>.\u{feff}"   SUBMITS, and the recorded row has no trailing U+FEFF
//! "…answer as VE-<number>.\u{3000}"   SUBMITS, and the recorded row has no trailing U+3000
//! "…answer as VD-<number>.\u{200b}"   SUBMITS, and the U+200B IS in the recorded row
//! "…answer as NL3-<number>.\u{85}"    SUBMITS, and the U+0085 IS in the recorded row
//! "…answer as NL8-<number>.\u{b}"     SUBMITS, and the recorded row ends `^K`
//! "…answer as NL9-<number>.\u{c}"     SUBMITS, and the recorded row ends `^L`
//! "   " / "\u{a0}" / "\n"             NO-OP: nothing is submitted, ever
//! "…answer as V1-<number>. \"         INSERTS A NEWLINE: nothing is submitted, ever
//! ```
//!
//! The last three rows were taken at 2.1.227 on 2026-08-11 against an isolated
//! composer (`docs/path-b-adversarial.md` sec. 12); the rest at 2.1.226 through
//! the shipped `pmuxd`, and the U+200B, U+FEFF, U+3000, space and newline rows
//! were re-taken at 2.1.227 and are unchanged.
//!
//! Two rules come out of that, and the second is a corollary of the first:
//!
//! 1. **The composer records the buffer with its trailing run of
//!    [`is_trimmed_from_the_end`] characters removed.** This paragraph gave that
//!    set two names — "JS `String.prototype.trimEnd`'s" and "White_Space plus
//!    U+FEFF" — and the table above now refutes BOTH: White_Space carries
//!    U+0085, which the composer **keeps**, and `trimEnd` strips U+000B and
//!    U+000C, which the composer **rewrites**. What is measured is one character
//!    at a time, and every edge is measured in both directions: U+FEFF is
//!    removed although White_Space does not contain it, U+200B is NOT removed
//!    although it is invisible and is an `is_ignorable_prompt_prefix` member,
//!    and U+0085 is NOT removed although it is whitespace by every property
//!    Rust has. Looking at JS was still right — the reader on the other end is a
//!    Node/Ink TUI, for the reason [`is_ignorable_prompt_prefix`] gives one
//!    paragraph down — but being a Node program does not put every string
//!    through `trimEnd`. Interior trailing whitespace
//!    (`"line one   \nNonce VF…"`) survives untouched, and so does LEADING
//!    whitespace: this is `trimEnd`, not `trim`.
//! 2. **A buffer that is empty once that run is removed is never submitted at
//!    all.** Enter is a no-op, the composer keeps holding the text, and pmux
//!    waits for an acknowledgement that cannot arrive -- 600 000 ms under daemon
//!    policy -- and then destroys the instance. Nothing here states this
//!    separately: [`crate::normalize_prompt`] applies rule 1, so such a prompt
//!    reaches the daemon's own empty-prompt refusal as the empty string.
//!
//! And one rule that is not a trim at all:
//!
//! 3. **A buffer whose last character is `\` is not submitted either.** `\`
//!    immediately before the cursor is Claude Code's multiline chord: Enter
//!    DELETES the backslash and inserts a newline. MEASURED on the screen --
//!    `❯ Nonce TX1. What is 3 plus 5? \` became `❯ Nonce TX1. What is 3 plus 5?`
//!    over a blank second row -- and MEASURED through pmux twice, once with one
//!    trailing backslash and once with two, both of which ran to the caller's
//!    deadline having written no `user` row at all. It is not an escaping rule:
//!    two backslashes fail exactly as one does, because what is tested is the
//!    character before the cursor. This one is REFUSED
//!    ([`ComposerRefusal::LineContinuation`]) rather than normalized, because
//!    removing it would change the text, and no other spelling of the prompt
//!    delivers it.
//!
//! # What the composer does with a PASTE, which is not what it does with a file
//!
//! pmux never types a prompt character by character; it writes one bracketed
//! paste and then one Enter (`crates/rmux/src/backend.rs`). Whether a composer
//! behaviour is reachable through pmux therefore depends on whether it survives
//! being pasted, and the answer is not the same for every one of them. MEASURED
//! at 2.1.226:
//!
//! * **A mode prefix fires through a paste.** With `'!'` removed from
//!   [`COMPOSER_MODE_PREFIXES`], `pmux ask '!echo … > /tmp/…'` left the recorded
//!   input-gate frame reading `!` U+00A0 `echo … > /tmp/…` over `! for shell
//!   mode` -- the `❯` glyph REPLACED, the `!` consumed. It fires even when the
//!   rest of the paste is collapsed: a five-line version of the same prompt
//!   rendered `!` U+00A0 `[Pasted text #1 +4 lines]`. The first character of a
//!   paste is read as a mode switch before anything else happens to it.
//! * **The `@` file picker does NOT fire through a paste.** Typed into an
//!   isolated `claude`, `@Non` opened a picker offering `Nonce-secrets.txt`;
//!   pasting the same characters did not. Through pmux, with a matching file
//!   planted in a live cell's own cwd, `@Nonce W9. What is 3 plus 5?` was
//!   recorded verbatim and answered. The picker is anchored at the cursor, and
//!   after a paste the cursor is at the end of the buffer rather than inside the
//!   `@` token.
//!
//! That distinction is why `@` is ordinary text here and `!` is not, and it is a
//! stronger statement than the one this module used to make. The old argument
//! was that a Path B cell's cwd is empty so the picker has nothing to match; it
//! was never measured, and it stops holding the moment a cell has a non-empty
//! cwd. The measured reason does not depend on the cwd at all -- it was taken
//! WITH a matching file present -- and it says which future composer behaviours
//! to worry about: the sticky ones, not the transient ones.

/// Every character the composer reads as a MODE SWITCH when it stands first in
/// an otherwise empty buffer, rather than as the first character of a prompt.
///
/// MEASURED at 2.1.226; see the module documentation for the sweep and for the
/// 30 swept punctuation characters that are NOT modes. Ordered as measured, `/`
/// first because it is the one pmux refused before `!` was found.
///
/// "Stands first" means first in the BUFFER, and the buffer is filled by one
/// bracketed paste: a mode prefix fires through a paste, which is the property
/// that makes this constant load-bearing rather than theoretical. The module
/// documentation has the frame.
pub const COMPOSER_MODE_PREFIXES: [char; 2] = ['/', '!'];

/// Every character the composer accepts and then records as something else.
///
/// Three members, each MEASURED as a recorded `user` row: U+0009 comes back as
/// four U+0020 (2.1.226), and U+000B and U+000C come back as the two ASCII
/// characters `^K` and `^L` (2.1.227, `docs/path-b-adversarial.md` sec. 12).
///
/// The last two were found by asking what the composer does with a prompt's
/// LAST character, and they were in [`is_trimmed_from_the_end`] until then --
/// which is to say pmux deleted them from the end of a caller's prompt while
/// refusing them everywhere else. They are rewrites and not removals: a
/// two-character replacement is nothing pmux can deliver by normalizing, so it
/// is refused here like the tab.
///
/// Separated from [`COMPOSER_MODE_PREFIXES`] because the failures are different
/// in kind -- a mode prefix changes what Enter DOES, a rewritten character only
/// changes what the transcript SAYS -- and an operator reading a refusal needs
/// to be told which happened.
pub const COMPOSER_REWRITTEN_CHARACTERS: [char; 3] = ['\t', '\u{b}', '\u{c}'];

/// The character the composer reads as a line continuation when it stands last.
///
/// Separate from [`COMPOSER_REWRITTEN_CHARACTERS`] because it is not a rewrite:
/// the transcript says nothing, since there is no transcript row. Enter deletes
/// this character and inserts a newline instead of submitting, so the turn is
/// never sent at all.
pub const COMPOSER_LINE_CONTINUATION: char = '\\';

/// Whether pmux refuses `character` WHEREVER it stands in a prompt.
///
/// One statement of "pmux will not paste this", read by both of the rules that
/// used to decide it separately: [`is_trimmed_from_the_end`] subtracts this set,
/// so a character pmux refuses can never be silently deleted instead, and
/// `crates/service/src/driver_io.rs`'s `validate_prompt` refuses on the same
/// property one line below the trim that used to hide it.
///
/// The set is every control character but `\n`, which is `validate_prompt`'s
/// own clause and not a new policy: pmux writes a prompt into a pseudoterminal
/// as one bracketed paste, and a control character in that payload is read by
/// something -- the terminal, the line discipline, or Ink -- before it is read
/// as text. `\n` is the exception because a multi-line prompt is ordinary and
/// MEASURED to survive. `\r` never reaches here: [`crate::normalize_prompt`]
/// has already folded it to `\n`.
///
/// # Why this is what the trim subtracts
///
/// Four characters were in both sets and the delete ran first, so the refusal
/// never fired for a caller who put one of them LAST: U+0009, U+000B, U+000C
/// and U+0085. Each was refused inside a prompt, with a message, and removed
/// from the end of one, without a word --
/// `paste_injection::a_character_refused_inside_a_prompt_is_refused_at_its_end_too`
/// is that statement over the guard chain, and it fails for every one of the
/// four before this subtraction.
#[must_use]
pub fn is_refused_wherever_it_stands(character: char) -> bool {
    character.is_control() && character != '\n'
}

/// Whether pmux removes `character` from the END of a caller's prompt.
///
/// Stated once, as a conjunction, because it is one rule and not two: **pmux
/// removes what the composer removes, less anything pmux refuses to paste.**
/// The first factor is a claim about Claude; the second is
/// [`is_refused_wherever_it_stands`], and it is what makes the trimmed set and
/// the refused set unable to disagree — the defect they used to have was that
/// they were written separately and overlapped in four characters.
///
/// # The first factor: White_Space plus U+FEFF, which is a superset
///
/// It is deliberately a SUPERSET of what the composer removes, and it is not
/// offered as the composer's own rule. Two spellings of that rule have been
/// tried here and both are wrong in a way this factor is not allowed to be:
/// White_Space ∪ {U+FEFF} exceeds JS `String.prototype.trimEnd`'s set by
/// U+0085, which is MEASURED KEPT; and `trimEnd`'s set in turn names U+000B and
/// U+000C, which the composer MEASURABLY does not remove either — it REWRITES
/// them. What every measurement so far supports is the direction: the composer
/// removes nothing this factor does not name.
///
/// So the factor is stated as the superset it is, and the conjunction below
/// subtracts the four characters it names wrongly. The reason JS is the right
/// place to have looked at all is the one [`is_ignorable_prompt_prefix`] gives
/// below: the reader on the other end is a Node/Ink TUI.
///
/// # U+0085 is MEASURED, and the answer is that the composer KEEPS it
///
/// This comment used to say the question was unanswerable without relaxing two
/// guards at once. It was answered on 2026-08-11 without relaxing either, by
/// driving an isolated `claude` 2.1.227 in a 120x24 `tmux` pane — the same
/// bracketed paste and the same Enter pmux writes — and reading the child's own
/// recorded `user` row: `…and nothing else.` U+0085 came back with the U+0085
/// on it, byte for byte (`… 65 6c 73 65 2e c2 85`), and the turn was answered.
/// **The composer records a trailing U+0085 verbatim; it does not remove it.**
/// An interior one is recorded verbatim too. `docs/path-b-adversarial.md`
/// sec. 12 carries the run.
///
/// So the trade this comment stated for three commits — *"trim it and silently
/// alter the caller's prompt, or keep it and refuse the prompt with a
/// message"* — had a false first branch. Trimming it was never matching the
/// composer; it was pmux deleting a character Claude would have kept. It is
/// refused now, by the control-character rule that already refused an interior
/// one, and the caller is told rather than answered on a prompt they did not
/// send.
///
/// # U+000B and U+000C are MEASURED too, and they are REWRITES
///
/// The same session sent each of them as a prompt's last character. Neither is
/// removed: the composer records `^K` and `^L`, two ASCII characters each,
/// exactly as it records a tab as four spaces. They are therefore
/// [`COMPOSER_REWRITTEN_CHARACTERS`] members and not trim-set members, and the
/// refusal a caller meets names what the composer does with them.
///
/// # It is deliberately NOT [`is_ignorable_prompt_prefix`]
///
/// That function is one line away and starts with the same `is_whitespace`
/// call. It is whitespace plus every Cf character, because it only decides
/// where a rule starts LOOKING and a superset there costs nothing. This one
/// decides which characters pmux may DROP from a caller's prompt, where a
/// superset is a silently truncated prompt: U+200B is a Cf character and was
/// MEASURED to survive in the recorded row — at 2.1.226 and again at 2.1.227 —
/// so a shared predicate would remove a character Claude keeps and arm exactly
/// the turn this module exists to prevent. Two rules that begin the same way
/// and end differently are two rules.
#[must_use]
pub fn is_trimmed_from_the_end(character: char) -> bool {
    (character.is_whitespace() || character == '\u{feff}')
        && !is_refused_wherever_it_stands(character)
}

/// The text the composer will RECORD for a buffer holding `prompt`.
///
/// The one statement of rule 1 above. [`crate::normalize_prompt`] applies it to
/// both ends of the equality a turn is proven by, and [`composer_refusal`]
/// applies it before asking what the last character is, so a prompt ending
/// `"\\   "` is judged on the `\` the composer will be left holding rather than
/// on the space a caller happened to type after it.
///
/// It answers for a prompt pmux will actually paste, which is the only kind
/// there is by the time either caller reaches it. For a prompt carrying a
/// character [`is_refused_wherever_it_stands`] names, this returns that
/// character rather than what the composer would have made of it -- deliberately,
/// because inventing the substitution is what a normalization must never do:
/// `"ok\t"` is refused with a message about four spaces, not silently turned
/// into `"ok"` or into `"ok    "`.
#[must_use]
pub fn composer_submitted_text(prompt: &str) -> &str {
    prompt.trim_end_matches(is_trimmed_from_the_end)
}

/// Whether `character` can stand before a composer mode prefix without the text
/// ceasing to be a mode switch.
///
/// `char::is_whitespace` is the White_Space property, and White_Space is not the
/// rule the reader on the other end applies. Claude Code is a Node/Ink TUI, and
/// JS `String.prototype.trim` strips U+FEFF, which White_Space does not contain:
/// `"\u{feff}/clear"` therefore passed a `trim_start().starts_with('/')` test
/// here and would then reach a JS-side command detector as `/clear`. That is a
/// caller-typed slash command, the exact thing this rule exists to make
/// impossible, and it arrives by accident — a BOM on Windows-authored text is
/// not an exotic input.
///
/// So the set is not "whitespace plus U+FEFF": patching one lookalike leaves the
/// next one. It is whitespace plus every Unicode format character (general
/// category Cf). Each of those is invisible, none can be the meaningful first
/// character of a prompt, and every one of them is a candidate for the same
/// stripped-before-parsing treatment U+FEFF just demonstrated. `is_control` is
/// no help here: Cf is not Cc, and U+FEFF is not a control character.
///
/// This only decides where the rule starts LOOKING. Nothing is removed from the
/// prompt.
#[must_use]
pub fn is_ignorable_prompt_prefix(character: char) -> bool {
    // Unicode 16 general category Cf, enumerated rather than pulled in as a
    // dependency: this is the only place in pmux that needs the property, and
    // an enumeration can be read against the standard.
    character.is_whitespace()
        || matches!(character,
            '\u{ad}'                    // SOFT HYPHEN
            | '\u{600}'..='\u{605}'     // Arabic subtending marks
            | '\u{61c}'                 // ARABIC LETTER MARK
            | '\u{6dd}'                 // ARABIC END OF AYAH
            | '\u{70f}'                 // SYRIAC ABBREVIATION MARK
            | '\u{890}'..='\u{891}'     // Arabic pound and piastre marks
            | '\u{8e2}'                 // ARABIC DISPUTED END OF AYAH
            | '\u{180e}'                // MONGOLIAN VOWEL SEPARATOR
            | '\u{200b}'..='\u{200f}'   // ZERO WIDTH SPACE .. RIGHT-TO-LEFT MARK
            | '\u{202a}'..='\u{202e}'   // bidi embedding and override
            | '\u{2060}'..='\u{2064}'   // WORD JOINER .. INVISIBLE PLUS
            | '\u{2066}'..='\u{206f}'   // bidi isolates and deprecated formats
            | '\u{feff}'                // ZERO WIDTH NO-BREAK SPACE (BOM)
            | '\u{fff9}'..='\u{fffb}'   // interlinear annotation
            | '\u{110bd}'               // KAITHI NUMBER SIGN
            | '\u{110cd}'               // KAITHI NUMBER SIGN ABOVE
            | '\u{13430}'..='\u{1343f}' // Egyptian hieroglyph format controls
            | '\u{1bca0}'..='\u{1bca3}' // shorthand format controls
            | '\u{1d173}'..='\u{1d17a}' // musical beam, slur and phrase formats
            | '\u{e0001}'               // LANGUAGE TAG
            | '\u{e0020}'..='\u{e007f}' // tag characters
        )
}

/// Why the composer would not record one prompt verbatim.
///
/// Carries the character that refused it, because "the prompt was refused" is
/// not an operator-actionable statement and "the leading `!` opens bash mode"
/// is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposerRefusal {
    /// The first meaningful character is a [`COMPOSER_MODE_PREFIXES`] member.
    ModePrefix(char),
    /// The prompt carries a [`COMPOSER_REWRITTEN_CHARACTERS`] member.
    RewrittenCharacter(char),
    /// What the composer would be left holding ends with
    /// [`COMPOSER_LINE_CONTINUATION`], so Enter would insert a newline rather
    /// than submit. Carries no character: there is exactly one of them, and a
    /// variant that carried `'\\'` would invite a second member to be added by
    /// editing a literal instead of by measuring one.
    LineContinuation,
}

impl ComposerRefusal {
    /// A stable slug, for a log line or a metric label.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ModePrefix(_) => "composer_mode_prefix",
            Self::RewrittenCharacter(_) => "composer_rewritten_character",
            Self::LineContinuation => "composer_line_continuation",
        }
    }

    /// What is wrong, and not what to do about it.
    ///
    /// Split from [`Self::remedy`] because the two travel separately: a daemon
    /// refusal carries the explanation in `message` and the remedy in
    /// `details.recommendation`, and `bin/pmux-mcp` renders the second and
    /// redacts the first. While these were one string the MCP surface rendered
    /// NEITHER, and a model reading "your prompt starts with `/`" and "Path B
    /// is not enabled" got byte-identical payloads.
    ///
    /// [`Self::describe`] still joins them, so every reader that wants the
    /// sentence gets the same sentence it always got.
    #[must_use]
    pub fn explain(self) -> String {
        match self {
            Self::ModePrefix('/') => "a prompt whose first character is `/` opens the composer's \
                 command menu, so Enter would select a command instead of sending the prompt; \
                 slash commands require a future typed control API."
                .to_owned(),
            Self::ModePrefix('!') => "a prompt whose first character is `!` switches the composer \
                 into bash mode, so Enter would RUN THE REST AS A SHELL COMMAND on the host \
                 instead of sending it to the model."
                .to_owned(),
            Self::ModePrefix(character) => format!(
                "a prompt whose first character is `{character}` switches the composer into a \
                 mode, so Enter would not send the prompt to the model."
            ),
            Self::RewrittenCharacter('\t') => "a prompt containing a tab (U+0009) is recorded by \
                 the composer as four spaces, so the turn pmux typed could never be acknowledged \
                 and the instance would be destroyed proving it."
                .to_owned(),
            // MEASURED at 2.1.227 rather than reasoned from the tab: each of
            // these comes back as the TWO ASCII characters of its caret
            // notation. The notation is not spelled out by a rule here, because
            // a rule would claim it for every control character and only these
            // two were sent.
            Self::RewrittenCharacter('\u{b}') => "a prompt containing a vertical tab (U+000B) is \
                 recorded by the composer as the two characters `^K`, so the turn pmux typed \
                 could never be acknowledged and the instance would be destroyed proving it."
                .to_owned(),
            Self::RewrittenCharacter('\u{c}') => "a prompt containing a form feed (U+000C) is \
                 recorded by the composer as the two characters `^L`, so the turn pmux typed \
                 could never be acknowledged and the instance would be destroyed proving it."
                .to_owned(),
            Self::RewrittenCharacter(character) => format!(
                "a prompt containing U+{:04X} is rewritten by the composer, so the turn pmux \
                 typed could never be acknowledged.",
                character as u32
            ),
            Self::LineContinuation => format!(
                "a prompt whose last character is `{COMPOSER_LINE_CONTINUATION}` is read by the \
                 composer as a line continuation, so Enter would INSERT A NEWLINE instead of \
                 sending the prompt: the turn would never be submitted and would run to its \
                 deadline, destroying the instance."
            ),
        }
    }

    /// What the caller changes to make this prompt sendable.
    ///
    /// EVERY variant answers, including the two general arms. The general
    /// `RewrittenCharacter` arm used to end at "could never be acknowledged"
    /// with no action at all, and the test that requires a remedy did not see
    /// it: `COMPOSER_REWRITTEN_CHARACTERS` held ONE character then, so the arm
    /// that runs for a second one had never been rendered. It holds three now,
    /// and this is the arm two of them take.
    #[must_use]
    pub const fn remedy(self) -> &'static str {
        match self {
            Self::ModePrefix('/') => "Put a word before it, or ask for the command as text.",
            Self::ModePrefix('!') => "Put a word before it, or escape it.",
            Self::ModePrefix(_) => "Put a word before it.",
            Self::RewrittenCharacter('\t') => "Send spaces.",
            Self::RewrittenCharacter(_) => "Send the prompt without that character.",
            Self::LineContinuation => {
                "Remove it -- doubling it does not escape it, and neither does a space after it, \
                 which the composer removes."
            }
        }
    }

    /// What is wrong AND what would be right, in one line.
    ///
    /// Single-sourced rather than restated at each call site: the daemon, the
    /// CLI and the MCP tool description all render this string, and three
    /// sentences about one rule is how the rule came to be described three
    /// different ways.
    #[must_use]
    pub fn describe(self) -> String {
        format!("{} {}", self.explain(), self.remedy())
    }
}

/// The single question every entry point that types a prompt has to ask.
///
/// `None` means the composer was MEASURED to hold this prompt's characters as
/// typed. It is not a promise about a Claude Code pmux has not measured; see the
/// module documentation for what a new mode character would do instead.
///
/// The mode test runs first because a mode prefix is the worse outcome and the
/// operator should be told about that one.
///
/// The line-continuation test runs last and over [`composer_submitted_text`]
/// rather than over `prompt`, so that it asks about the character the composer
/// will actually be holding when Enter arrives. There is deliberately no test
/// here for a prompt that is entirely trimmed away: [`crate::normalize_prompt`]
/// turns that one into the empty string, and an empty prompt is already refused
/// by every entry point that calls this. A second rule stating the same thing is
/// how a boundary comes to have two guards that disagree.
#[must_use]
pub fn composer_refusal(prompt: &str) -> Option<ComposerRefusal> {
    if let Some(character) = prompt
        .trim_start_matches(is_ignorable_prompt_prefix)
        .chars()
        .next()
        .filter(|character| COMPOSER_MODE_PREFIXES.contains(character))
    {
        return Some(ComposerRefusal::ModePrefix(character));
    }
    if let Some(character) = prompt
        .chars()
        .find(|character| COMPOSER_REWRITTEN_CHARACTERS.contains(character))
    {
        return Some(ComposerRefusal::RewrittenCharacter(character));
    }
    composer_submitted_text(prompt)
        .ends_with(COMPOSER_LINE_CONTINUATION)
        .then_some(ComposerRefusal::LineContinuation)
}

/// The MEASURED opening of the placeholder the composer renders in place of a
/// paste it decided not to show. Written once and used by both the builder and
/// the reader below, so there is one spelling of it rather than two that can
/// drift.
const PASTE_PLACEHOLDER_OPEN: &str = "[Pasted text #";

/// The placeholder Claude Code 2.1.226 renders for a collapsed paste of
/// `line_breaks` line breaks, taken as the `counter`-th paste of that process.
///
/// Every part of this except `counter` is derived from the prompt, which is the
/// point: the reader below rebuilds the expected string and compares it for
/// equality instead of pattern-matching a shape and trusting the number inside
/// it.
fn collapsed_paste_placeholder(counter: u64, line_breaks: usize) -> String {
    if line_breaks == 0 {
        format!("{PASTE_PLACEHOLDER_OPEN}{counter}]")
    } else {
        format!("{PASTE_PLACEHOLDER_OPEN}{counter} +{line_breaks} lines]")
    }
}

/// What the composer's rendered rows prove about the prompt pmux pasted.
///
/// The two variants are deliberately not interchangeable, and a caller that
/// treats them as one bool is giving up the distinction that matters: one of
/// them has the prompt's own bytes on the screen and the other has a sentence
/// saying there is a paste.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposerRenderProof {
    /// The rows ARE the prompt, byte for byte, from its first character to its
    /// last — with only the row breaks and the characters a terminal cannot
    /// draw between them.
    PromptText,
    /// The one row is the placeholder the composer substitutes for a paste it
    /// collapsed, and the line count that placeholder declares is this prompt's
    /// own. **Not one character of the prompt is on the screen.** This is the
    /// weaker variant and it exists because the screen carries nothing else:
    /// see the module documentation's table.
    CollapsedPaste,
}

/// The characters a rendered row is allowed to be missing at a row boundary or
/// at the end of the buffer.
///
/// Two independent reasons, and the set is the union of exactly those two:
///
/// * **The terminal right-trims a row**, so trailing whitespace a caller sent
///   never reaches the screen. MEASURED: `"a   b   "` renders as `a   b`.
/// * **The wrap consumes the character it breaks at.** MEASURED at 2.1.226 on a
///   120-column pane: the 274-character prompt in
///   [`tests::measured_composer_renders`] broke after `…made long enough`, and
///   the next character of the prompt is the space the break ate.
///
/// [`is_ignorable_prompt_prefix`] is whitespace plus general category Cf, and
/// Cf is here for a third reason that is the same shape: those characters are
/// invisible, so a row cannot show them either. `docs/path-b-adversarial.md`
/// sec. 11 MEASURED a prompt of nothing but U+200B rendering as a blank row.
fn a_rendered_row_can_omit(character: char) -> bool {
    is_ignorable_prompt_prefix(character)
}

/// Advance `remaining` past `row`, allowing a break before it.
///
/// The shortest run of omittable characters that makes `row` match is taken.
/// It is a choice and not a deduction, and it is wrong only for a prompt whose
/// break lands inside a run of whitespace AND whose next row therefore begins
/// with whitespace — which needs a single unbroken whitespace run wider than
/// the pane, since a word wrap never starts a row with the space it just ate.
fn advance_past<'a>(remaining: &'a str, row: &str) -> Option<&'a str> {
    let mut rest = remaining;
    loop {
        if let Some(tail) = rest.strip_prefix(row) {
            return Some(tail);
        }
        let next = rest.chars().next()?;
        if !a_rendered_row_can_omit(next) {
            return None;
        }
        rest = &rest[next.len_utf8()..];
    }
}

/// Whether the composer's rendered rows can be this prompt.
///
/// `rows` is the composer's own text: the first row with the `❯` and its single
/// separating cell removed, and every row below it with the continuation gutter
/// removed, in order. `None` is the answer that matters — the composer is
/// showing something that is neither this prompt nor a collapsed paste of this
/// prompt's shape, and the caller must not press Enter on it.
///
/// # Why the whole buffer, and not a head
///
/// Until 2026-08-10 this took the FIRST ROW ONLY and asked
/// `prompt.starts_with(head)`, which has no lower bound. PROBED at `8c3d387`:
/// a composer showing `W` proved the prompt `What is 2 plus 2?`, Enter went in,
/// and the post-Enter equality then refused the turn and destroyed the pooled
/// instance. One delivered character satisfied the clause the gate was named
/// for.
///
/// The rows are the WHOLE buffer, which is what makes the stronger question
/// answerable at all: `active_editor` takes them from the `❯` anchor through
/// the cursor row, the anchor is the buffer's first row and the cursor sits at
/// its last character — MEASURED at the end of the text on all 12 renders
/// below — so nothing the composer holds is off the bottom. A buffer too tall
/// for the pane loses its anchor and is refused by the gate above, exactly as
/// it already was.
///
/// # The two repairs that a measurement refutes
///
/// Neither of the obvious lower bounds survives contact with the composer:
///
/// * *"A head shorter than the row means the composer did not wrap, so require
///   equality with the first line."* The 274-character prompt renders 114
///   characters on a row that holds 116, because the wrap broke at a word
///   boundary. This refuses every wrapping prompt.
/// * *"Require the row to be full to within one word."* MEASURED on a
///   600-character prompt: the third row ends 8 columns short of a width its
///   neighbours reach, with a 7-character word next. No greedy-fill model
///   admits that render, and this rule refuses it.
///
/// This function needs neither, because it never asks how wide the row is. It
/// asks only whether the rows, in order, spell the prompt.
///
/// # What is still NOT proven
///
/// [`ComposerRenderProof::CollapsedPaste`] proves the shape of a paste and not
/// one character of its text; the module documentation's table says why the
/// screen carries nothing else. And a row that is missing an INTERIOR
/// invisible character is refused rather than admitted — `"a\u{200b}b"` renders
/// as `ab` and does not match — which is the same refusal this rule made
/// before, filed in `docs/path-b-adversarial.md` sec. 11.4.
#[must_use]
pub fn composer_render_proof(rows: &[&str], prompt: &str) -> Option<ComposerRenderProof> {
    let (&head, continuations) = rows.split_first()?;

    // The verbatim reading first, so a prompt that literally begins with the
    // placeholder text is judged as its own text rather than as a collapse.
    let mut remaining = prompt.strip_prefix(head);
    for row in continuations {
        remaining = remaining.and_then(|rest| advance_past(rest, row));
    }
    if remaining.is_some_and(|rest| rest.chars().all(a_rendered_row_can_omit)) {
        return Some(ComposerRenderProof::PromptText);
    }

    // A collapsed paste is ONE row: MEASURED at 2.1.226 for prompts of 4, 5, 8,
    // 12, 20 and 41 lines and for single lines of 1000, 1600, 2400 and 3021
    // characters, every one of them a single `[Pasted text #k]` row with the
    // cursor at its end.
    if !continuations.is_empty() {
        return None;
    }
    let counter: u64 = head
        .strip_prefix(PASTE_PLACEHOLDER_OPEN)?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()?;
    (head == collapsed_paste_placeholder(counter, prompt.matches('\n').count()))
        .then_some(ComposerRenderProof::CollapsedPaste)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// THE FIRST MEASUREMENT. The second is `measured_composer_renders` below,
    /// and between them they are the only literals in this file that are
    /// measurements.
    ///
    /// Each row is one first character sent through a real `pmux ask` against
    /// Claude Code 2.1.226 on a warm Path B instance, with the child's own
    /// recorded `user` row read back and compared to the bytes pmux sent.
    /// `true` means the composer took it as a mode switch and did not record
    /// the prompt; `false` means the row was byte-identical and the model
    /// answered. `docs/path-b-adversarial.md` sec. 4.4 carries the run.
    ///
    /// `/` is the one row not established by that sweep, because pmux already
    /// refused it and no turn could be spent on it: it is measured instead by
    /// the command-menu capture in
    /// `crates/service/src/driver_io.rs::prove_control_command_selection`,
    /// where a typed `/c` left `/cd` highlighted and Enter ran `/cd`.
    ///
    /// The shipped constants are checked AGAINST this table rather than
    /// iterated over, which is the difference between a test that catches a
    /// character being dropped from the guard and one that shrinks with it.
    ///
    /// The last five rows -- `@ ) ] } _` -- were added on 2026-08-09 and cost
    /// five real turns. They are the ones `PUNCTUATION_THE_SWEEP_DID_NOT_SEND`
    /// used to declare, and the reason for sending them is that a declaration
    /// with a reason is still not a measurement. `@` was the one that mattered:
    /// it opens a file picker when TYPED, and the argument that Path B's cwd is
    /// empty was never the reason it is safe here. It was sent with a matching
    /// file planted in the live cell's own cwd and was recorded verbatim.
    const MEASURED_FIRST_CHARACTER_SWEEP: [(char, bool); 32] = [
        ('/', true),
        ('!', true),
        ('#', false),
        ('$', false),
        ('%', false),
        ('>', false),
        ('?', false),
        ('\\', false),
        ('|', false),
        ('~', false),
        ('^', false),
        ('&', false),
        ('*', false),
        ('-', false),
        ('+', false),
        ('.', false),
        (',', false),
        (':', false),
        (';', false),
        ('=', false),
        ('`', false),
        ('"', false),
        ('\'', false),
        ('(', false),
        ('[', false),
        ('{', false),
        ('<', false),
        ('@', false),
        (')', false),
        (']', false),
        ('}', false),
        ('_', false),
    ];

    /// The shipped set is exactly the measured one -- neither short nor long.
    ///
    /// Short is the defect this module was written for: `!` was missing and a
    /// `pmux ask` ran a shell command. Long would be pmux refusing prompts a
    /// composer holds perfectly well.
    #[test]
    fn the_shipped_mode_prefixes_are_exactly_the_ones_measured_as_modes() {
        let measured: Vec<char> = MEASURED_FIRST_CHARACTER_SWEEP
            .into_iter()
            .filter_map(|(character, is_mode)| is_mode.then_some(character))
            .collect();
        assert_eq!(
            COMPOSER_MODE_PREFIXES.to_vec(),
            measured,
            "COMPOSER_MODE_PREFIXES must be exactly the characters the sweep measured as modes"
        );
    }

    /// The ASCII punctuation characters the sweep never sent, and why each was
    /// left out.
    ///
    /// This table exists because the module documentation used to say the sets
    /// were "complete for 2.1.226 over ASCII punctuation" while the sweep held
    /// 27 of 32. An omission with a reason is a decision; an omission implied
    /// away by a sentence is the house bug class. Every entry here is
    /// **UNMEASURED** — none of these characters has been sent as a first
    /// character at any version — and none is claimed to be safe.
    ///
    /// **It is now empty, and that is the result rather than a tidy-up.** The
    /// five characters it held (`@ ) ] } _`) were sent on 2026-08-09 and are
    /// rows of `MEASURED_FIRST_CHARACTER_SWEEP`. The table is kept at length
    /// zero, and `the_sweep_accounts_for_every_ascii_punctuation_character`
    /// still consults it, because the guarantee worth having is not "this table
    /// is empty" but "every character is in exactly one of the two", and a
    /// future alphabet with a character that cannot be sent needs a place to say
    /// so that is not a paragraph.
    const PUNCTUATION_THE_SWEEP_DID_NOT_SEND: [(char, &str); 0] = [];

    /// Every ASCII punctuation character is either swept or declared unswept.
    ///
    /// The set is DERIVED from `char::is_ascii_punctuation` rather than
    /// restated, so the alphabet cannot quietly disagree with the sentence
    /// describing it. Dropping a row from the sweep without declaring it, or
    /// declaring one that is also swept, both fail here.
    #[test]
    fn the_sweep_accounts_for_every_ascii_punctuation_character() {
        let swept: BTreeSet<char> = MEASURED_FIRST_CHARACTER_SWEEP
            .into_iter()
            .map(|(character, _)| character)
            .collect();
        let declared: BTreeSet<char> = PUNCTUATION_THE_SWEEP_DID_NOT_SEND
            .into_iter()
            .map(|(character, _)| character)
            .collect();
        assert_eq!(
            swept.len(),
            MEASURED_FIRST_CHARACTER_SWEEP.len(),
            "the sweep names a character twice"
        );
        assert_eq!(
            declared.len(),
            PUNCTUATION_THE_SWEEP_DID_NOT_SEND.len(),
            "the unswept table names a character twice"
        );
        let overlap: Vec<char> = swept.intersection(&declared).copied().collect();
        assert!(
            overlap.is_empty(),
            "{overlap:?} is both swept and declared unswept"
        );
        let alphabet: BTreeSet<char> = (0u8..=127)
            .map(char::from)
            .filter(char::is_ascii_punctuation)
            .collect();
        let accounted: BTreeSet<char> = swept.union(&declared).copied().collect();
        let unaccounted: Vec<char> = alphabet.difference(&accounted).copied().collect();
        assert!(
            unaccounted.is_empty(),
            "{unaccounted:?} are ASCII punctuation and appear in neither table; a \
             character the sweep did not send is a gap that must be DECLARED with \
             a reason, not left for the prose to imply away"
        );
        let stray: Vec<char> = accounted.difference(&alphabet).copied().collect();
        assert!(
            stray.is_empty(),
            "{stray:?} are in a punctuation table and are not ASCII punctuation"
        );
        for (character, reason) in PUNCTUATION_THE_SWEEP_DID_NOT_SEND {
            assert!(
                reason.len() > 20,
                "{character:?} is declared unswept without a reason"
            );
        }
    }

    /// The reproduction, as a predicate, over every row of the sweep and every
    /// invisible the rule reads past.
    #[test]
    fn the_sweep_replays_through_every_invisible_prefix() {
        let invisibles = ["", " ", "\n", "\u{feff}", "\u{200f}", "\u{2066}", "  \n "];
        for (character, is_mode) in MEASURED_FIRST_CHARACTER_SWEEP {
            for invisible in invisibles {
                let prompt = format!("{invisible}{character}What is 1 plus 1?");
                let expected = is_mode.then_some(ComposerRefusal::ModePrefix(character));
                assert_eq!(
                    composer_refusal(&prompt),
                    expected,
                    "{prompt:?} must follow the measured sweep"
                );
            }
        }
    }

    #[test]
    fn a_tab_anywhere_is_refused_and_named() {
        for prompt in ["A\tB", "ask\n\tindented", "\ttrailing question"] {
            assert_eq!(
                composer_refusal(prompt),
                Some(ComposerRefusal::RewrittenCharacter('\t')),
                "{prompt:?} carries a tab the composer rewrites"
            );
        }
    }

    /// A mode prefix and a tab in one prompt reports the mode prefix: it is the
    /// one that changes what Enter does.
    #[test]
    fn the_mode_prefix_is_reported_ahead_of_the_rewrite() {
        assert_eq!(
            composer_refusal("!echo\thi"),
            Some(ComposerRefusal::ModePrefix('!'))
        );
    }

    #[test]
    fn an_ordinary_prompt_is_admitted() {
        assert_eq!(composer_refusal("What is 1 plus 1?"), None);
        assert_eq!(composer_refusal("Nonce KX41. Add 17 and 25."), None);
        // Not first: a mode character mid-prompt is text, MEASURED.
        assert_eq!(composer_refusal("Add 1 and 1.\n!echo hi"), None);
        assert_eq!(composer_refusal("Add 1 and 1.\n/clear"), None);
    }

    /// What one character standing LAST in a buffer becomes in the child's own
    /// recorded `user` row.
    ///
    /// Three answers and not two, which is the finding: a bool could not say
    /// what U+000B and U+000C do.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum RecordedTail {
        /// Absent from the recorded row: the composer removed it.
        Removed,
        /// Present in the recorded row, byte for byte.
        Kept,
        /// Replaced by other characters, which is neither of the above and is
        /// the reason this is an enum.
        RewrittenAs(&'static str),
    }

    /// THE THIRD MEASUREMENT: what the composer does to the END of a buffer.
    ///
    /// One row per character sent as the LAST character of an otherwise
    /// ordinary nonce-arithmetic prompt, with the child's own recorded `user`
    /// row read back. The first six were taken at Claude Code 2.1.226 through a
    /// real `pmux ask` (`docs/path-b-adversarial.md` sec. 11) and RE-TAKEN
    /// unchanged at 2.1.227; the last three are 2.1.227, against an isolated
    /// composer driven with the same bracketed paste and the same Enter, and
    /// are sec. 12.
    ///
    /// The rows that are not `Removed` are the reason this is a measurement and
    /// not `char::is_whitespace`. U+FEFF is removed and is not White_Space.
    /// U+200B is kept and is invisible. **U+0085 is kept and IS White_Space** —
    /// the row that took three commits to send, because reaching a composer
    /// with a C1 control through pmux needs two guards relaxed and reaching one
    /// without pmux needs neither. U+000B and U+000C are rewritten, so no
    /// trailing-trim rule of any shape describes them.
    const MEASURED_LAST_CHARACTER_SWEEP: [(char, RecordedTail); 9] = [
        (' ', RecordedTail::Removed),
        ('\n', RecordedTail::Removed),
        ('\u{a0}', RecordedTail::Removed),
        ('\u{3000}', RecordedTail::Removed),
        ('\u{feff}', RecordedTail::Removed),
        ('\u{200b}', RecordedTail::Kept),
        ('\u{85}', RecordedTail::Kept),
        ('\u{b}', RecordedTail::RewrittenAs("^K")),
        ('\u{c}', RecordedTail::RewrittenAs("^L")),
    ];

    /// The shipped trim rule is exactly the measured one, in both directions.
    ///
    /// Checked against the table rather than iterated out of it, for the same
    /// reason `the_shipped_mode_prefixes_are_exactly_the_ones_measured_as_modes`
    /// is: a test that derives the rule from the rule shrinks with it.
    ///
    /// `Kept` and `RewrittenAs` are both `false` here and they are not the same
    /// fact. A kept character pmux must DELIVER; a rewritten one pmux must
    /// REFUSE, because there is no text it could type that the composer would
    /// record as what the caller wrote. The second half of that is asserted
    /// through `composer_refusal`, so the table decides both rules rather than
    /// one rule and a paragraph.
    #[test]
    fn the_shipped_trailing_trim_is_exactly_the_one_measured() {
        for (character, tail) in MEASURED_LAST_CHARACTER_SWEEP {
            let is_trimmed = tail == RecordedTail::Removed;
            assert_eq!(
                is_trimmed_from_the_end(character),
                is_trimmed,
                "U+{:04X} must follow the measured last-character sweep",
                character as u32
            );
            let prompt = format!("Nonce V0. What is 3 plus 5?{character}");
            if let RecordedTail::RewrittenAs(recorded) = tail {
                let refusal = composer_refusal(&prompt)
                    .expect("a character the composer rewrites is refused, never delivered");
                assert_eq!(refusal, ComposerRefusal::RewrittenCharacter(character));
                assert!(
                    refusal.explain().contains(recorded),
                    "U+{:04X} is recorded as {recorded:?} and the refusal does not say so: {}",
                    character as u32,
                    refusal.explain()
                );
            }
            let expected = if is_trimmed {
                "Nonce V0. What is 3 plus 5?".to_owned()
            } else {
                prompt.clone()
            };
            assert_eq!(
                composer_submitted_text(&prompt),
                expected,
                "U+{:04X} at the end of a prompt",
                character as u32
            );
        }
    }

    /// Every code point JS `String.prototype.trimEnd` removes, MEASURED by
    /// running it rather than read off the specification.
    ///
    /// ```text
    /// node -e 'let s=[];for(let c=0;c<0x110000;c++){const t=String.fromCodePoint(c);
    ///   if(("a"+t).trimEnd()==="a") s.push(c);}
    ///   console.log(s.length, s.map(c=>c.toString(16)).join(" "))'
    /// -> 25 9 a b c d 20 a0 1680 2000 2001 2002 2003 2004 2005 2006 2007 2008
    ///       2009 200a 2028 2029 202f 205f 3000 feff
    /// ```
    ///
    /// Node 25.x, macOS 15.7.7 / aarch64, 2026-08-10. It is here because the
    /// composer's own rule was DERIVED from it -- Claude Code is a Node/Ink TUI
    /// -- and a derivation whose source is a sentence cannot be checked.
    ///
    /// The derivation is not exact, and [`MEASURED_LAST_CHARACTER_SWEEP`] is
    /// where that shows: `trimEnd` strips U+000B and U+000C, and the composer
    /// RECORDS both, as `^K` and `^L`. Being a Node program does not make every
    /// string in it go through `trimEnd` -- which is the argument for measuring
    /// each character rather than adopting this table as the rule.
    const MEASURED_JS_TRIM_END_SET: [char; 25] = [
        '\u{9}', '\u{a}', '\u{b}', '\u{c}', '\u{d}', '\u{20}', '\u{a0}', '\u{1680}', '\u{2000}',
        '\u{2001}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}',
        '\u{2008}', '\u{2009}', '\u{200a}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}',
        '\u{3000}', '\u{feff}',
    ];

    /// The two spellings that used to disagree now agree, and they agree
    /// because every character they differed on is one pmux refuses.
    ///
    /// The doc comment on [`is_trimmed_from_the_end`] once gave two spellings
    /// for one set -- "JS `String.prototype.trimEnd`'s set" and "White_Space
    /// plus U+FEFF" -- and they are different sets, by exactly U+0085. That
    /// difference is now MEASURED (the composer keeps it) and it is no longer
    /// load-bearing either way, because the shipped rule subtracts everything
    /// [`is_refused_wherever_it_stands`] names and U+0085 is in it.
    ///
    /// Both derivations are run, from the enumerated sets rather than from the
    /// predicate, so an edit that moves the predicate fails here with the code
    /// points named -- and so does an edit that reintroduces the disagreement
    /// by widening either side.
    #[test]
    fn the_shipped_trim_set_is_both_spellings_less_what_pmux_refuses() {
        let universe = || (0..=char::MAX as u32).filter_map(char::from_u32);
        let js: BTreeSet<char> = MEASURED_JS_TRIM_END_SET.into_iter().collect();
        assert_eq!(
            js.len(),
            MEASURED_JS_TRIM_END_SET.len(),
            "the table repeats"
        );
        let white_space_or_bom: BTreeSet<char> = universe()
            .filter(|character| character.is_whitespace() || *character == '\u{feff}')
            .collect();
        let shipped: BTreeSet<char> = universe()
            .filter(|character| is_trimmed_from_the_end(*character))
            .collect();
        let describe = |set: &BTreeSet<char>| {
            set.iter()
                .map(|character| format!("U+{:04X}", *character as u32))
                .collect::<Vec<_>>()
        };
        let deliverable = |set: &BTreeSet<char>| -> BTreeSet<char> {
            set.iter()
                .copied()
                .filter(|character| !is_refused_wherever_it_stands(*character))
                .collect()
        };

        // The disagreement, stated: it is one character, and it is the one the
        // sweep now carries as MEASURED KEPT.
        assert_eq!(
            describe(&white_space_or_bom.difference(&js).copied().collect()),
            ["U+0085"],
            "White_Space plus U+FEFF may exceed JS's set only by U+0085",
        );
        assert!(
            js.is_subset(&white_space_or_bom),
            "JS trims a character neither White_Space nor U+FEFF names: {:?}",
            describe(&js.difference(&white_space_or_bom).copied().collect()),
        );
        assert!(
            MEASURED_LAST_CHARACTER_SWEEP
                .iter()
                .any(|(character, tail)| *character == '\u{85}' && *tail == RecordedTail::Kept),
            "U+0085 must stay in the sweep as MEASURED KEPT: it is what makes \
             the difference above harmless rather than unknown",
        );

        // ...and once what pmux refuses is subtracted, the two spellings are
        // the same set, and that set is the shipped one.
        assert_eq!(
            shipped,
            deliverable(&js),
            "shipped is not JS's set, less what pmux refuses"
        );
        assert_eq!(
            shipped,
            deliverable(&white_space_or_bom),
            "shipped is not White_Space plus U+FEFF, less what pmux refuses",
        );

        // The half that protects a pooled instance: a character JS trims and
        // pmux does not is a prompt whose recorded row cannot equal the armed
        // one. Every one of them is refused instead -- U+0009, U+000B and
        // U+000C by name, and U+000D only in principle, since
        // `normalize_prompt` folds it to `\n` before any guard sees it.
        let kept_by_pmux: Vec<char> = js.difference(&shipped).copied().collect();
        assert_eq!(
            describe(&kept_by_pmux.iter().copied().collect()),
            ["U+0009", "U+000B", "U+000C", "U+000D"]
        );
        assert!(
            kept_by_pmux
                .iter()
                .all(|character| is_refused_wherever_it_stands(*character)),
            "pmux keeps a character JS trims and does not refuse it: {:?}",
            describe(&kept_by_pmux.iter().copied().collect()),
        );
    }

    /// It is `trimEnd`, not `trim`, and not a per-line trim.
    ///
    /// MEASURED: `"   Nonce VA…"` was recorded with its three leading spaces and
    /// `"line one   \nNonce VF…"` was recorded with the three spaces before its
    /// newline. Both would be destroyed by a rule that reached any further.
    #[test]
    fn the_trim_reaches_the_end_of_the_buffer_and_nowhere_else() {
        assert_eq!(
            composer_submitted_text("   Nonce VA. What is 3 plus 5?"),
            "   Nonce VA. What is 3 plus 5?"
        );
        assert_eq!(
            composer_submitted_text("line one   \nNonce VF. What is 3 plus 5?"),
            "line one   \nNonce VF. What is 3 plus 5?"
        );
        assert_eq!(
            composer_submitted_text("line one   \nNonce VF.  \n\u{feff} "),
            "line one   \nNonce VF."
        );
    }

    /// A buffer that is nothing but trimmed characters submits nothing at all,
    /// and this rule is the whole of what pmux needs to know about that.
    ///
    /// MEASURED three ways at 2.1.226 -- `"   "`, `"\u{a0}"` and `"\n"` each ran
    /// to the caller's deadline having written no `user` row -- and confirmed on
    /// the screen of an isolated `claude`, where Enter left the three spaces
    /// sitting in the composer. Nothing in this module refuses them: they come
    /// out of the trim as the empty string and meet the empty-prompt refusal
    /// every entry point already has.
    #[test]
    fn a_buffer_of_nothing_but_trimmed_characters_is_left_empty() {
        for prompt in ["   ", "\u{a0}", "\n", "\n\n  \u{feff}\u{3000}"] {
            assert!(
                composer_submitted_text(prompt).is_empty(),
                "{prompt:?} must trim to nothing"
            );
        }
        // ...and the one invisible the composer keeps is NOT trimmed to
        // nothing, so it stays a prompt rather than becoming an empty one.
        assert_eq!(composer_submitted_text("\u{200b}"), "\u{200b}");
    }

    /// A trailing backslash is refused, and it is refused for what the composer
    /// will be HOLDING rather than for what the caller typed.
    ///
    /// MEASURED at 2.1.226: one trailing backslash and two trailing backslashes
    /// each ran to the caller's deadline with no `user` row written, and the
    /// screen showed the `\` deleted and a newline inserted in its place. It is
    /// not an escaping rule, which is why the second case is here.
    #[test]
    fn a_trailing_line_continuation_is_refused_and_named() {
        for prompt in [
            "Nonce V1. What is 3 plus 5? \\",
            "Nonce V5. What is 3 plus 5? \\\\",
            "\\",
            // The trim runs first, so a space after the backslash does not save
            // it: the composer removes the space and is left holding the `\`.
            "Nonce V7. What is 3 plus 5? \\   ",
            "Nonce V8. What is 3 plus 5? \\\n",
        ] {
            assert_eq!(
                composer_refusal(prompt),
                Some(ComposerRefusal::LineContinuation),
                "{prompt:?} leaves the composer holding a trailing `\\`"
            );
        }
    }

    /// A backslash that is not last is ordinary text, MEASURED both as a first
    /// character and mid-prompt. Over-refusing here would refuse every prompt
    /// about Windows paths, TeX or escaping.
    #[test]
    fn a_backslash_that_is_not_last_is_admitted() {
        for prompt in [
            "Nonce V2. a\\\\b What is 3 plus 5?",
            "Nonce V6. one \\\nWhat is 3 plus 5?",
            "\\What is 1 plus 1?",
            "C:\\Users\\me is a path. What is 1 plus 1?",
        ] {
            assert_eq!(
                composer_refusal(prompt),
                None,
                "{prompt:?} is ordinary text"
            );
        }
    }

    /// Every refusal the rule can return names a remedy.
    ///
    /// The refusals are DERIVED by running `composer_refusal` over prompts built
    /// out of the three constants, rather than by listing the variants: a
    /// character added to a constant brings its own case, and a variant that no
    /// constant can produce is not a refusal a caller can meet.
    ///
    /// The remedy is no longer a list of substrings kept beside the messages:
    /// it is [`ComposerRefusal::remedy`], and `describe` is that string joined
    /// to [`ComposerRefusal::explain`]. So "every refusal names a remedy" is
    /// now checked against the value the daemon actually publishes in
    /// `details.recommendation` rather than against four phrases that happened
    /// to appear somewhere in a sentence.
    ///
    /// The two GENERAL arms are asserted separately at the end, because no
    /// constant can reach them: `COMPOSER_MODE_PREFIXES` and
    /// `COMPOSER_REWRITTEN_CHARACTERS` each have a per-character arm for every
    /// member they hold today, so the arm that runs for the next member added
    /// has never been rendered by this test. One of them shipped with no remedy
    /// at all.
    #[test]
    fn every_refusal_says_what_would_be_right() {
        let mut refusals = Vec::new();
        for character in COMPOSER_MODE_PREFIXES {
            refusals.push(composer_refusal(&format!("{character}echo hi")));
        }
        for character in COMPOSER_REWRITTEN_CHARACTERS {
            refusals.push(composer_refusal(&format!(
                "A{character}B. What is 1 plus 1?"
            )));
        }
        refusals.push(composer_refusal(&format!(
            "What is 1 plus 1? {COMPOSER_LINE_CONTINUATION}"
        )));
        let refusals: Vec<ComposerRefusal> = refusals
            .into_iter()
            .map(|refusal| refusal.expect("each shape is built to be refused"))
            .collect();
        assert_eq!(
            refusals.len(),
            COMPOSER_MODE_PREFIXES.len() + COMPOSER_REWRITTEN_CHARACTERS.len() + 1
        );

        // Including the two arms no constant reaches, which is where the one
        // remedy-less variant was hiding. `\u{b}` stood here until it became a
        // measured member of the rewritten set and stopped being an unreached
        // arm; the two characters below are asserted to be outside both
        // constants rather than believed to be, because that is exactly the
        // fact this block depends on and exactly the one that expired.
        let unreachable_today = [
            ComposerRefusal::ModePrefix('%'),
            ComposerRefusal::RewrittenCharacter('\u{7f}'),
        ];
        for refusal in unreachable_today {
            match refusal {
                ComposerRefusal::ModePrefix(character) => assert!(
                    !COMPOSER_MODE_PREFIXES.contains(&character),
                    "{character:?} is a mode prefix now, so this arm is reached above"
                ),
                ComposerRefusal::RewrittenCharacter(character) => assert!(
                    !COMPOSER_REWRITTEN_CHARACTERS.contains(&character),
                    "{character:?} is a rewritten character now, so this arm is reached above"
                ),
                ComposerRefusal::LineContinuation => unreachable!("there is one of those"),
            }
        }
        for refusal in refusals.iter().copied().chain(unreachable_today) {
            let (explained, remedy) = (refusal.explain(), refusal.remedy());
            assert!(
                !remedy.trim().is_empty(),
                "{:?} refuses a caller and names no remedy",
                refusal.code()
            );
            // The remedy is an instruction, so it is a sentence of its own and
            // not a clause the explanation already contains: the daemon
            // publishes the two in different fields and a remedy that is part
            // of the explanation would be rendered twice on one surface and
            // never on the other.
            assert!(
                !explained.contains(remedy),
                "{:?}'s remedy is inside its explanation: {explained:?}",
                refusal.code()
            );
            assert_eq!(
                refusal.describe(),
                format!("{explained} {remedy}"),
                "describe must stay the two halves joined, so every existing reader is unchanged"
            );
        }
    }

    /// **No character pmux refuses is also one it deletes**, over the whole of
    /// Unicode.
    ///
    /// The invariant the two sets used to break. `is_trimmed_from_the_end` is
    /// written as the conjunction that makes it hold, so this cannot fail while
    /// that conjunction stands -- which is the point of writing it that way, and
    /// the reason this test is worth the line it takes: removing the second
    /// factor turns it red with the four characters named, and those four are
    /// exactly the ones a caller used to have deleted from the end of a prompt
    /// that would have been refused one place to the left.
    ///
    /// The end-to-end statement, through the guard chain a caller meets, is
    /// `crates/service/tests/paste_injection.rs`'s
    /// `a_character_refused_inside_a_prompt_is_refused_at_its_end_too`. This one
    /// is the same rule where both sets live.
    #[test]
    fn no_character_pmux_refuses_is_also_one_it_deletes() {
        let universe = || (0..=char::MAX as u32).filter_map(char::from_u32);
        let both: Vec<String> = universe()
            .filter(|character| {
                is_trimmed_from_the_end(*character) && is_refused_wherever_it_stands(*character)
            })
            .map(|character| format!("U+{:04X}", character as u32))
            .collect();
        assert!(
            both.is_empty(),
            "{both:?} would be refused inside a prompt and deleted from its end"
        );

        // The one control character pmux still removes is the newline, and it
        // is the one the refusal exempts. Stated as the whole intersection
        // rather than as an assertion about `\n`, so a trim set that grew a
        // second control character would fail here even if the refusal grew
        // with it.
        let control_and_trimmed: Vec<char> = universe()
            .filter(|character| is_trimmed_from_the_end(*character) && character.is_control())
            .collect();
        assert_eq!(control_and_trimmed, vec!['\n']);

        // Every character the trim can reach is one `is_ignorable_prompt_prefix`
        // reads past. That containment is what lets the end-to-end test derive
        // its domain from those two predicates instead of listing characters.
        let outside: Vec<char> = universe()
            .filter(|character| {
                is_trimmed_from_the_end(*character) && !is_ignorable_prompt_prefix(*character)
            })
            .collect();
        assert!(
            outside.is_empty(),
            "{outside:?} are trimmed and are not whitespace or Cf"
        );
    }

    /// U+0085, the character this module got wrong for three commits, in every
    /// position a caller can put it.
    ///
    /// MEASURED at 2.1.227 (`docs/path-b-adversarial.md` sec. 12): the composer
    /// records a trailing U+0085 verbatim and answers the turn. pmux therefore
    /// does not delete it -- deleting it was never matching the composer -- and
    /// what a caller meets instead is the control-character refusal that always
    /// applied to an interior one, in both positions and with the same message.
    #[test]
    fn a_trailing_next_line_is_no_longer_deleted_from_a_prompt() {
        const NEL: char = '\u{85}';
        assert!(NEL.is_control(), "the whole rule rests on this");
        assert!(
            NEL.is_whitespace(),
            "...and so does the trap it used to fall into"
        );
        assert!(!is_trimmed_from_the_end(NEL));
        assert!(is_refused_wherever_it_stands(NEL));
        for prompt in [
            format!("ask me{NEL}"),
            format!("ask{NEL}me"),
            NEL.to_string(),
        ] {
            assert_eq!(
                composer_submitted_text(&prompt),
                prompt,
                "{prompt:?} must reach the daemon's refusal with its U+0085 still on it"
            );
        }
    }

    /// THE SECOND MEASUREMENT: `(the composer's rendered rows with the `❯`,
    /// separator and continuation gutters already removed, the prompt that
    /// produced them, what they prove)`.
    ///
    /// Claude Code 2.1.226, macOS 15.7.7 / aarch64, 24x120 pane, read out of the
    /// frames the input gate itself recorded at site `input_gate.post_paste`.
    /// The prompts are reproduced by construction where they were generated by
    /// one, so a reader can check the line-break arithmetic instead of trusting
    /// a transcribed count.
    ///
    /// **This table used to disagree with itself and nothing could tell.** The
    /// wrapping render was recorded here as `long_wrapping[..118]` and in
    /// `crates/service/src/driver_io.rs` as the same prompt cut at 114, and both
    /// passed, because the rule under them accepted ANY non-empty prefix. It is
    /// 114: re-measured on 2026-08-10 by driving the shipped `pmuxd` with
    /// `PMUX_SCREEN_CORPUS_DIR` set and reading the frame, the row is
    /// `❯` U+00A0 then `Answer with only the number: … made long enough`, 114
    /// characters, broken at the word boundary before `that`.
    ///
    /// Eight of the twelve rows were re-measured or measured for the first time
    /// in that session. The four that were not — the 3-line prompt's second and
    /// third rows aside, the 20-line, 41-line and 3021-character collapses —
    /// are carried over, and the 41-line row is the only one no run this
    /// session reproduced.
    fn measured_composer_renders() -> Vec<(Vec<String>, String, Option<ComposerRenderProof>)> {
        fn rows(rows: &[&str]) -> Vec<String> {
            rows.iter().map(|row| (*row).to_owned()).collect()
        }

        let long_wrapping = "Answer with only the number: what is the sum of 2 and 2, given that \
                             this sentence is deliberately made long enough that it must wrap onto \
                             more than one rendered composer row on any ordinary terminal pane \
                             width, so that the wrapping behaviour of the composer can be recorded?";
        // MEASURED: 200 `x` after a sentence, so the break lands inside a
        // single word and the wrap has no whitespace to consume.
        let unbroken_word = format!("Reply with only the word OK. {}", "x".repeat(200));
        // MEASURED: `padding word ` repeated and cut at 600, which renders six
        // rows. The third row stops 8 columns short of a width its neighbours
        // reach, which is the render that refutes every greedy-fill model.
        let six_rows = format!(
            "Reply with only the word PAD. {}",
            "padding word ".repeat(200)
        );
        let six_rows = six_rows[..600].to_owned();
        let four_lines = "Reply with only the word OK.\nfiller 2\nfiller 3\nfiller 4";
        let twenty_lines = format!("Reply with only the word OK.{}", "\nfiller".repeat(19));
        let forty_one_lines = format!(
            "Reply with only the word MANY.{}",
            "\nfiller line, ignore it".repeat(40)
        );
        let three_thousand = format!(
            "Reply with only the word LONG. {}",
            "padding word ".repeat(230)
        );
        vec![
            (
                rows(&["Reply with the single word FOUR and nothing else."]),
                "Reply with the single word FOUR and nothing else.".to_owned(),
                Some(ComposerRenderProof::PromptText),
            ),
            (
                rows(&["Reply with only the word OK."]),
                "Reply with only the word OK.".to_owned(),
                Some(ComposerRenderProof::PromptText),
            ),
            (
                rows(&["日本語と絵文字🙂を含むプロンプトです。Reply with only the word WIDE."]),
                "日本語と絵文字🙂を含むプロンプトです。Reply with only the word WIDE.".to_owned(),
                Some(ComposerRenderProof::PromptText),
            ),
            // A CJK line long enough to wrap: the break is at the row edge, 58
            // characters of 2 columns each, and it consumed nothing.
            (
                rows(&[
                    "日本語のみで構成された非常に長い一行のプロンプトです。これは折り返しの挙動を測定するためのものです。列数と文字数が異",
                    "なることを確かめます。ここまで読んだら OK とだけ答えてください。",
                ]),
                "日本語のみで構成された非常に長い一行のプロンプトです。これは折り返しの挙動を測定するためのものです。列数と文字数が異なることを確かめます。ここまで読んだら OK とだけ答えてください。"
                    .to_owned(),
                Some(ComposerRenderProof::PromptText),
            ),
            (
                rows(&[
                    "Reply with only the word THREE.",
                    "Ignore this second line.",
                    "And this third line.",
                ]),
                "Reply with only the word THREE.\nIgnore this second line.\nAnd this third line."
                    .to_owned(),
                Some(ComposerRenderProof::PromptText),
            ),
            // A prompt line of its own that BEGINS WITH WHITESPACE: the gutter
            // is two cells and the prompt's four spaces are rendered after it.
            (
                rows(&[
                    "Reply with only the word THREE.",
                    "    this third line begins with four spaces",
                    "last line",
                ]),
                "Reply with only the word THREE.\n    this third line begins with four spaces\nlast line"
                    .to_owned(),
                Some(ComposerRenderProof::PromptText),
            ),
            (
                rows(&[&long_wrapping[..114], &long_wrapping[115..231], &long_wrapping[232..]]),
                long_wrapping.to_owned(),
                Some(ComposerRenderProof::PromptText),
            ),
            (
                rows(&[&unbroken_word[..116], &unbroken_word[116..]]),
                unbroken_word.clone(),
                Some(ComposerRenderProof::PromptText),
            ),
            (
                rows(&[
                    &six_rows[..115],
                    &six_rows[116..232],
                    &six_rows[233..341],
                    &six_rows[342..458],
                    &six_rows[459..570],
                    &six_rows[571..],
                ]),
                six_rows.clone(),
                Some(ComposerRenderProof::PromptText),
            ),
            (
                rows(&["[Pasted text #6 +3 lines]"]),
                four_lines.to_owned(),
                Some(ComposerRenderProof::CollapsedPaste),
            ),
            (
                rows(&["[Pasted text #5 +19 lines]"]),
                twenty_lines,
                Some(ComposerRenderProof::CollapsedPaste),
            ),
            (
                rows(&["[Pasted text #1 +40 lines]"]),
                forty_one_lines,
                Some(ComposerRenderProof::CollapsedPaste),
            ),
            (
                rows(&["[Pasted text #7]"]),
                three_thousand,
                Some(ComposerRenderProof::CollapsedPaste),
            ),
        ]
    }

    #[test]
    fn every_measured_render_proves_its_own_prompt_and_by_the_named_route() {
        for (rows, prompt, expected) in measured_composer_renders() {
            let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
            assert_eq!(
                composer_render_proof(&rows, &prompt),
                expected,
                "measured render {rows:?}"
            );
        }
    }

    /// THE FINDING: the rule had no lower bound, so one delivered character
    /// proved a whole prompt.
    ///
    /// PROBED at `8c3d387` through the gate itself: a composer showing `W`
    /// proved `What is 2 plus 2?`, Enter went in, and the post-Enter equality
    /// then refused the turn and destroyed the pooled instance.
    ///
    /// Every prefix of a measured render is checked, not just the one-character
    /// one, because "a prefix is not the prompt" is the whole rule and a test of
    /// its first character would be the same understatement as the guard it
    /// replaces. The two real renders below are the boundary: a prompt that
    /// wraps has a first row that IS a strict prefix, and it is admitted only
    /// with the rows that finish it.
    #[test]
    fn a_prefix_of_this_prompt_is_not_this_prompt() {
        let prompt = "What is 2 plus 2?";
        for cut in 1..prompt.len() {
            assert_eq!(
                composer_render_proof(&[&prompt[..cut]], prompt),
                None,
                "a composer holding {:?} proved {prompt:?}",
                &prompt[..cut]
            );
        }
        assert_eq!(
            composer_render_proof(&[prompt], prompt),
            Some(ComposerRenderProof::PromptText)
        );

        // The same, on the measured wrapping render: its first row alone proves
        // nothing, and it proves the prompt when the rows that finish it are
        // there.
        let (rows, wrapping, expected) = measured_composer_renders()
            .into_iter()
            .find(|(rows, _, _)| rows.len() == 3 && rows[0].starts_with("Answer with only"))
            .expect("the measured wrapping render");
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        assert_eq!(composer_render_proof(&rows[..1], &wrapping), None);
        assert_eq!(composer_render_proof(&rows[..2], &wrapping), None);
        assert_eq!(composer_render_proof(&rows, &wrapping), expected);
    }

    /// The finding this rule exists for: geometry cannot tell one composer's
    /// contents from another's, and the text can.
    #[test]
    fn rows_that_are_not_this_prompts_rows_prove_nothing() {
        assert_eq!(
            composer_render_proof(&["! echo PWNED > /tmp/x"], "What is 2 plus 2?"),
            None
        );
        // Rows longer than the prompt, which is the case a `starts_with`
        // written the other way round would have admitted.
        assert_eq!(composer_render_proof(&["hello world"], "hello"), None);
        // One character in.
        assert_eq!(composer_render_proof(&["hellp"], "hello"), None);
        // Text that IS in the prompt and is not its beginning, which is the
        // case a `contains` would have admitted and neither line above can see:
        // `"hello".contains("hello world")` and `"hello".contains("hellp")` are
        // both false. The prompt is this module's own reproduction with the
        // shell command appended, so the composer showing the command alone --
        // a real screen, one row, every geometric clause satisfied -- is
        // refused for the reason this proof exists.
        assert_eq!(
            composer_render_proof(
                &["! echo PWNED > /tmp/x"],
                "What is 2 plus 2? ! echo PWNED > /tmp/x"
            ),
            None
        );
        assert_eq!(
            composer_render_proof(&["plus 2?"], "What is 2 plus 2?"),
            None
        );
        // A continuation row that continues something else.
        assert_eq!(
            composer_render_proof(&["What is 2", "plus 3?"], "What is 2 plus 2?"),
            None
        );
        // Rows in the wrong order.
        assert_eq!(
            composer_render_proof(&["plus 2?", "What is 2"], "What is 2 plus 2?"),
            None
        );
        // No rows at all is not a proof of anything, including of the empty
        // prompt: an editor with no rendered row resolved no editor.
        assert_eq!(composer_render_proof(&[], "What is 2 plus 2?"), None);
        assert_eq!(composer_render_proof(&[], ""), None);
        // A prompt that is a prefix of a DIFFERENT collapsed paste's line count.
        assert_eq!(
            composer_render_proof(&["[Pasted text #2 +4 lines]"], "one\ntwo\nthree"),
            None
        );
        // The counter is per process and unpredictable, but the rest of the
        // placeholder is not free-form.
        for wrong in [
            "[Pasted text #] ",
            "[Pasted text #two +2 lines]",
            "[Pasted text #2 +2 line]",
            "[Pasted text #2 2 lines]",
            "[Pasted text #2 +2 lines",
            "[Pasted text #2 +2 lines] and more",
        ] {
            assert_eq!(
                composer_render_proof(&[wrong], "one\ntwo\nthree"),
                None,
                "{wrong:?}"
            );
        }
    }

    /// MEASURED: a collapsed paste is ONE row. A placeholder with anything
    /// under it is a screen pmux has never seen, and the rows below it are not
    /// the prompt either -- the prompt is not on the screen at all.
    #[test]
    fn a_collapsed_paste_is_the_only_row() {
        let prompt = "one\ntwo\nthree";
        assert_eq!(
            composer_render_proof(&["[Pasted text #2 +2 lines]"], prompt),
            Some(ComposerRenderProof::CollapsedPaste)
        );
        for below in ["", "two", "[Pasted text #3 +2 lines]"] {
            assert_eq!(
                composer_render_proof(&["[Pasted text #2 +2 lines]", below], prompt),
                None,
                "a placeholder with {below:?} under it"
            );
        }
    }

    /// The counter is the only part of the placeholder that is not derived, so
    /// every value of it is admitted and nothing else is.
    #[test]
    fn the_paste_counter_is_free_and_the_line_count_is_not() {
        let prompt = "head\ntwo\nthree\nfour";
        for counter in [0_u64, 1, 7, 42, u64::from(u32::MAX), u64::MAX] {
            assert_eq!(
                composer_render_proof(&[&format!("[Pasted text #{counter} +3 lines]")], prompt),
                Some(ComposerRenderProof::CollapsedPaste),
                "counter {counter}"
            );
            for wrong_count in [0_usize, 2, 4, 30] {
                assert_eq!(
                    composer_render_proof(
                        &[&format!("[Pasted text #{counter} +{wrong_count} lines]")],
                        prompt
                    ),
                    None,
                    "counter {counter} line count {wrong_count}"
                );
            }
        }
    }

    /// MEASURED: the ` +n lines` clause is ABSENT when a paste has no line
    /// breaks, so the two forms are not interchangeable in either direction.
    #[test]
    fn the_two_placeholder_forms_do_not_substitute_for_each_other() {
        assert_eq!(
            composer_render_proof(&["[Pasted text #3]"], "one single line, no breaks"),
            Some(ComposerRenderProof::CollapsedPaste)
        );
        assert_eq!(
            composer_render_proof(&["[Pasted text #3 +0 lines]"], "one single line, no breaks"),
            None
        );
        assert_eq!(
            composer_render_proof(&["[Pasted text #3]"], "one\ntwo"),
            None
        );
    }

    /// A blank row is consistent with a blank line and with nothing else.
    ///
    /// This used to be the rule's largest hole rather than its smallest case: a
    /// blank first row proved any prompt whose FIRST LINE was blank, however
    /// many lines followed it, because nothing below the first row was
    /// compared. `"\nsecond line has text"` was proven by an empty composer.
    #[test]
    fn a_blank_composer_row_only_proves_a_blank_line() {
        assert_eq!(
            composer_render_proof(&[""], "   "),
            Some(ComposerRenderProof::PromptText)
        );
        assert_eq!(
            composer_render_proof(&["", "second line has text"], "\nsecond line has text"),
            Some(ComposerRenderProof::PromptText)
        );
        assert_eq!(composer_render_proof(&[""], "\nsecond line has text"), None);
        assert_eq!(composer_render_proof(&[""], "hello"), None);
        assert_eq!(composer_render_proof(&[""], " hello"), None);
    }

    /// A rendered row is right-trimmed and a wrap eats the character it breaks
    /// at, so the prompt may carry characters the screen cannot: whitespace,
    /// and the invisible ones.
    ///
    /// The direction matters. What a row may be missing is only ever a
    /// character that CANNOT BE DRAWN — a trailing space the terminal trimmed,
    /// the space a wrap consumed, a zero-width character. Anything a terminal
    /// would have drawn and did not is a composer holding something else.
    #[test]
    fn a_row_may_be_missing_only_what_a_terminal_cannot_draw() {
        assert_eq!(
            composer_render_proof(&["a   b"], "a   b   "),
            Some(ComposerRenderProof::PromptText)
        );
        assert_eq!(
            composer_render_proof(&["a   b   "], "a   b   "),
            Some(ComposerRenderProof::PromptText)
        );
        // MEASURED (`docs/path-b-adversarial.md` sec. 11): a trailing U+200B is
        // NOT trimmed from the prompt and is NOT rendered, and that turn is
        // answered. Refusing it here would cost an instance for a prompt the
        // composer records perfectly.
        assert_eq!(
            composer_render_proof(&["ask me"], "ask me\u{200b}"),
            Some(ComposerRenderProof::PromptText)
        );
        assert_eq!(
            composer_render_proof(&["one", "two"], "one\u{feff}\ntwo"),
            Some(ComposerRenderProof::PromptText)
        );
        // ...and one visible character is not.
        assert_eq!(composer_render_proof(&["ask me"], "ask me."), None);
        assert_eq!(composer_render_proof(&["one", "two"], "one.\ntwo"), None);
    }
}
