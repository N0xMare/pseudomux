//! What happens when a caller's prompt contains the bracketed-paste terminator.
//!
//! # The risk
//!
//! pmux injects every prompt as `\e[200~<text>\e[201~`
//! (`pseudomux_rmux::bracketed_paste_payload`). A terminal ends a bracketed
//! paste at the FIRST `\e[201~` it sees. A real consumer scans for exactly that
//! -- see `read_bracketed_paste` in `crates/e2e/src/bin/pmux-test-claude.rs`,
//! which is modelled byte-for-byte by [`decode_bracketed_paste`] below -- and
//! returns everything before the terminator as pasted text, leaving the
//! remainder in the input stream.
//!
//! That remainder is then read as KEYSTROKES. The composer they land in is one
//! `/` away from a live command menu whose entries include `/logout`, `/exit`,
//! `/config` and `/clear`. So `\e[201~` inside a caller's prompt is, absent a
//! guard, a caller-controlled path to executing a command the caller never
//! named and pmux never typed.
//!
//! # The required outcome
//!
//! For every input, pmux must do exactly one of two things:
//!
//! * **refuse**, or
//! * **submit exactly that text** -- byte-for-byte, with nothing left over to be
//!   read as keystrokes.
//!
//! Never anything else. [`every_input_is_refused_or_submitted_exactly`] is that
//! statement, checked against a real kernel PTY rather than against a model of
//! one, because the failure being hunted is a disagreement between what pmux
//! thinks it wrote and what a terminal actually does with those bytes.
//!
//! # Normalization is the one permitted transformation
//!
//! `validate_prompt` canonicalizes line endings (`\r\n` and lone `\r` both
//! become `\n`), composes to NFC, and removes the trailing run the composer
//! itself removes, before anything else runs -- so "exactly that text" means
//! `normalize_prompt(input)`.
//! [`normalization_folds_line_endings_and_composes_canonically_and_does_nothing_else`]
//! pins that down, so the escape hatch cannot quietly widen.
//!
//! That link named `normalization_only_rewrites_carriage_returns` until
//! 2026-08-09, and no test by that name has existed since NFC joined the rule:
//! the sentence had outlived both the test it cited and the number of
//! transformations it claimed.

// A real PTY is the point of this file: the guard being tested is a claim about
// what a terminal does with pmux's bytes, and a hand-rolled model of a terminal
// would be testing the model. `openpty`, `tcsetattr` and `close` are FFI.
#![allow(unsafe_code)]

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

use proptest::prelude::*;
use pseudomux_claude::{is_trimmed_from_the_end, normalize_prompt};
use pseudomux_rmux::bracketed_paste_payload;
use pseudomux_service::driver_io::{MAX_PROMPT_BYTES, validate_prompt};

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// What pmux did with one prompt. These are the only two acceptable outcomes,
/// and the type is what makes "never anything else" checkable rather than
/// aspirational.
#[derive(Debug, Eq, PartialEq)]
enum Outcome {
    /// A guard rejected the prompt. Nothing was written to any terminal.
    Refused(String),
    /// The exact wire bytes pmux would write.
    Wire(String),
}

/// Runs the full production guard chain in production order.
///
/// Both guards, in the order the service calls them: `validate_prompt` is the
/// service's policy filter over caller bytes, and `bracketed_paste_payload` is
/// the wire format's own precondition inside the terminal backend. Testing
/// either alone would prove nothing about the path a caller actually takes.
fn guard_chain(prompt: &str) -> Outcome {
    match validate_prompt(prompt) {
        Err(failure) => Outcome::Refused(format!("validate_prompt: {}", failure.message)),
        Ok(normalized) => match bracketed_paste_payload(&normalized) {
            Err(error) => Outcome::Refused(format!("paste: {error}")),
            Ok(wire) => Outcome::Wire(wire),
        },
    }
}

// ---------------------------------------------------------------------------
// A real PTY
// ---------------------------------------------------------------------------

/// A kernel pseudoterminal pair. pmux writes to the master; Claude Code reads
/// from the slave.
struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
}

impl Pty {
    /// Opens a pair and puts the slave in raw mode.
    ///
    /// Raw mode is not a convenience: Claude Code is a full-screen TUI and sets
    /// it, and the line discipline is exactly what would otherwise rewrite the
    /// bytes under test. `ICRNL` alone would turn a `\r` into a `\n` in the
    /// kernel and make a passing round trip meaningless.
    fn open() -> std::io::Result<Self> {
        let mut master: RawFd = -1;
        let mut slave: RawFd = -1;
        // SAFETY: both out-parameters are valid writable ints; the three
        // optional in-parameters are passed as NULL, which openpty documents as
        // "use the defaults".
        let opened = unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if opened != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openpty returned success, so both are freshly opened fds this
        // process now owns exclusively.
        let pty = unsafe {
            Self {
                master: OwnedFd::from_raw_fd(master),
                slave: OwnedFd::from_raw_fd(slave),
            }
        };

        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `slave` is a valid tty fd and `termios` is valid writable
        // storage of exactly the right type.
        if unsafe { libc::tcgetattr(slave, termios.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: tcgetattr succeeded, so the struct is initialized.
        let mut termios = unsafe { termios.assume_init() };
        // SAFETY: `termios` is an initialized struct owned by this frame.
        unsafe { libc::cfmakeraw(&raw mut termios) };
        // SAFETY: same, and `slave` is still a valid tty fd.
        if unsafe { libc::tcsetattr(slave, libc::TCSANOW, &raw const termios) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(pty)
    }
}

/// Writes `wire` into a real PTY and decodes what the far side receives.
///
/// The write runs on its own thread because a payload larger than the kernel's
/// PTY buffer blocks the writer until the reader drains it -- and "a very long
/// single line" is one of the inputs under test, so the deadlock is reachable
/// rather than theoretical.
fn pty_round_trip(wire: &str) -> std::io::Result<Decoded> {
    let pty = Pty::open()?;
    let mut master = std::fs::File::from(pty.master);
    let mut slave = std::fs::File::from(pty.slave);

    let payload = wire.as_bytes().to_vec();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        master.write_all(&payload)?;
        // The Enter that production sends after the paste, as a separate write,
        // exactly as `TerminalSession::enter` does. It is what makes "left over
        // to be read as keystrokes" observable: anything the decoder finds
        // between the terminator and this byte is residue.
        master.write_all(b"\r")?;
        master.flush()?;
        // Hold the master open until the reader is done; dropping it early
        // delivers EOF and the reader cannot distinguish that from a short
        // paste.
        std::thread::sleep(Duration::from_millis(50));
        Ok(())
    });

    let decoded = decode_bracketed_paste(&mut slave);
    let write_result = writer.join().expect("the PTY writer thread panicked");
    write_result?;
    decoded
}

#[derive(Debug)]
struct Decoded {
    /// Bytes the terminal would deliver as pasted text.
    pasted: Vec<u8>,
    /// Bytes left in the stream after the terminator, minus the single `\r`
    /// production sends as Enter. Anything here is read as KEYSTROKES.
    residue: Vec<u8>,
}

/// The bracketed-paste reader a real consumer implements.
///
/// Deliberately a copy of `read_bracketed_paste` from
/// `crates/e2e/src/bin/pmux-test-claude.rs` rather than a stricter parser: the
/// question this file answers is what a terminal that scans for the terminator
/// does with pmux's bytes, so a reader that was cleverer than the real one would
/// hide the very failure being hunted.
fn decode_bracketed_paste(source: &mut std::fs::File) -> std::io::Result<Decoded> {
    let mut prefix = [0_u8; 6];
    source.read_exact(&mut prefix)?;
    assert_eq!(
        prefix, PASTE_START,
        "pmux must open every injection with a bracketed-paste start"
    );

    let mut pasted = Vec::new();
    let mut candidate: Vec<u8> = Vec::new();
    loop {
        let mut byte = [0_u8; 1];
        source.read_exact(&mut byte)?;
        candidate.push(byte[0]);
        while !PASTE_END.starts_with(&candidate) {
            pasted.push(candidate.remove(0));
            if pasted.len() > MAX_PROMPT_BYTES + PASTE_END.len() {
                return Err(std::io::Error::other("paste exceeded the service limit"));
            }
        }
        if candidate == PASTE_END {
            break;
        }
    }

    // Whatever is still queued after the terminator. Production sends exactly
    // one `\r`; anything beyond that came out of the caller's prompt.
    let mut residue = Vec::new();
    let mut scratch = [0_u8; 4096];
    loop {
        let mut poll = libc::pollfd {
            fd: source.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: a valid one-element pollfd array with a live fd.
        let ready = unsafe { libc::poll(&raw mut poll, 1, 100) };
        if ready <= 0 || poll.revents & libc::POLLIN == 0 {
            break;
        }
        let read = source.read(&mut scratch)?;
        if read == 0 {
            break;
        }
        residue.extend_from_slice(&scratch[..read]);
    }
    // Strip the single Enter production is entitled to send.
    if residue.first() == Some(&b'\r') {
        residue.remove(0);
    }
    Ok(Decoded { pasted, residue })
}

// ---------------------------------------------------------------------------
// The hostile corpus
// ---------------------------------------------------------------------------

/// Every input class named in the threat model, plus the ones that look benign.
fn hostile_prompts() -> Vec<(&'static str, String)> {
    let mut cases: Vec<(&'static str, String)> = vec![
        // The headline risk: the terminator itself, in every position.
        ("terminator_only", "\x1b[201~".to_owned()),
        ("terminator_leading", "\x1b[201~rm -rf /".to_owned()),
        ("terminator_trailing", "hello\x1b[201~".to_owned()),
        (
            "terminator_middle",
            "hello\x1b[201~ and then /logout".to_owned(),
        ),
        (
            "terminator_then_slash_command",
            "benign\x1b[201~/logout\r".to_owned(),
        ),
        (
            "terminator_then_clear",
            "benign\x1b[201~/clear\r".to_owned(),
        ),
        ("terminator_doubled", "a\x1b[201~b\x1b[201~c".to_owned()),
        // The opener, which would nest or confuse a paste state machine.
        ("opener_embedded", "hello\x1b[200~world".to_owned()),
        ("opener_and_terminator", "\x1b[200~x\x1b[201~".to_owned()),
        // Bare escapes and CSI sequences.
        ("bare_esc", "hello\x1bworld".to_owned()),
        ("esc_only", "\x1b".to_owned()),
        ("csi_clear_screen", "hello\x1b[2Jworld".to_owned()),
        ("csi_cursor_home", "\x1b[H\x1b[2J".to_owned()),
        ("osc_title", "\x1b]0;pwned\x07".to_owned()),
        ("dcs", "\x1bPquery\x1b\\".to_owned()),
        // C0 controls.
        ("nul", "hello\0world".to_owned()),
        ("bel", "hello\x07world".to_owned()),
        ("backspace", "hello\x08world".to_owned()),
        ("vertical_tab", "hello\x0bworld".to_owned()),
        ("form_feed", "hello\x0cworld".to_owned()),
        ("ctrl_c", "hello\x03world".to_owned()),
        ("ctrl_d", "hello\x04world".to_owned()),
        ("ctrl_u", "hello\x15world".to_owned()),
        ("delete", "hello\x7fworld".to_owned()),
        // C1 controls, including the single-byte CSI that is the 8-bit spelling
        // of `\e[`.
        ("c1_csi", "hello\u{9b}201~world".to_owned()),
        ("c1_string_terminator", "hello\u{9c}world".to_owned()),
        // Line endings, which normalization is allowed to rewrite.
        ("newline", "line one\nline two".to_owned()),
        ("crlf", "line one\r\nline two".to_owned()),
        ("lone_cr", "line one\rline two".to_owned()),
        ("trailing_newline", "prompt\n".to_owned()),
        ("many_newlines", "a\n\n\n\nb".to_owned()),
        ("tab", "a\tb".to_owned()),
        // Slash-command lookalikes: the guard that keeps a caller from typing a
        // command directly.
        ("slash_clear", "/clear".to_owned()),
        ("slash_logout", "/logout".to_owned()),
        ("slash_leading_space", "   /clear".to_owned()),
        ("slash_leading_newline", "\n/clear".to_owned()),
        ("slash_bom", "\u{feff}/clear".to_owned()),
        ("slash_zwsp", "\u{200b}/clear".to_owned()),
        ("slash_rtl_mark", "\u{200f}/clear".to_owned()),
        ("slash_word_joiner", "\u{2060}/clear".to_owned()),
        ("slash_soft_hyphen", "\u{ad}/clear".to_owned()),
        ("slash_tag_char", "\u{e0020}/clear".to_owned()),
        ("slash_mid_text", "please run /clear for me".to_owned()),
        // Unicode shapes that change how many CELLS a string occupies without
        // changing how many chars it has.
        ("combining_marks", "e\u{301}\u{302}\u{303}llo".to_owned()),
        ("double_width", "\u{1f600}\u{1f4a9} wide".to_owned()),
        ("cjk", "\u{4f60}\u{597d}\u{4e16}\u{754c}".to_owned()),
        ("rtl_text", "\u{5e9}\u{5dc}\u{5d5}\u{5dd}".to_owned()),
        (
            "zwj_family",
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f466}".to_owned(),
        ),
        ("prompt_glyph", "\u{276f} looks like a composer".to_owned()),
        // Benign controls.
        ("plain", "what is 2 + 2?".to_owned()),
        ("quotes_and_backslashes", r#"a "b" \c\ 'd'"#.to_owned()),
    ];
    // Length extremes. The PTY buffer is a few kilobytes, so the long cases
    // exercise the concurrent-write path rather than just the guard.
    cases.push(("long_single_line", "x".repeat(96 * 1024)));
    cases.push(("long_with_newlines", "line\n".repeat(16 * 1024)));
    cases.push((
        "long_with_terminator_at_end",
        format!("{}\x1b[201~", "x".repeat(64 * 1024)),
    ));
    cases.push(("empty", String::new()));
    cases.push(("whitespace_only", "   \n\t  ".to_owned()));
    // The shapes the composer's own submit rule turns into something else, each
    // MEASURED at 2.1.226 as a destroyed instance before it was guarded
    // (`docs/path-b-adversarial.md` sec. 11).
    cases.push(("trailing_spaces", "what is 2 + 2?   ".to_owned()));
    cases.push(("trailing_nbsp", "what is 2 + 2?\u{a0}".to_owned()));
    cases.push(("trailing_bom", "what is 2 + 2?\u{feff}".to_owned()));
    cases.push(("trailing_zwsp_is_kept", "what is 2 + 2?\u{200b}".to_owned()));
    cases.push(("trailing_backslash", "what is 2 + 2? \\".to_owned()));
    cases.push((
        "trailing_backslash_doubled",
        "what is 2 + 2? \\\\".to_owned(),
    ));
    cases.push(("backslash_then_space", "what is 2 + 2? \\ ".to_owned()));
    cases
}

// ---------------------------------------------------------------------------
// The properties
// ---------------------------------------------------------------------------

/// **The whole requirement, over the hostile corpus.**
///
/// Every input is either refused, or submitted byte-for-byte with nothing left
/// over. The PTY round trip is what makes the second half a measurement instead
/// of a restatement of the encoder.
#[test]
fn every_input_is_refused_or_submitted_exactly() {
    let mut refused = 0_usize;
    let mut submitted = 0_usize;
    for (name, prompt) in hostile_prompts() {
        match guard_chain(&prompt) {
            Outcome::Refused(_) => refused += 1,
            Outcome::Wire(wire) => {
                let expected = normalize_prompt(&prompt);
                let decoded = pty_round_trip(&wire)
                    .unwrap_or_else(|error| panic!("{name}: PTY round trip failed: {error}"));
                assert_eq!(
                    String::from_utf8_lossy(&decoded.pasted),
                    expected,
                    "{name}: the terminal received text that is not the prompt pmux accepted"
                );
                assert!(
                    decoded.residue.is_empty(),
                    "{name}: {} bytes survived the paste and would be read as KEYSTROKES: {:?}",
                    decoded.residue.len(),
                    String::from_utf8_lossy(
                        &decoded.residue.iter().copied().take(64).collect::<Vec<_>>()
                    )
                );
                submitted += 1;
            }
        }
    }
    assert!(
        refused > 0 && submitted > 0,
        "the corpus must exercise both outcomes; got {refused} refused and {submitted} submitted"
    );
}

/// The headline risk, named on its own so a regression reads as what it is.
///
/// Any prompt containing the terminator must be REFUSED. It is not enough for
/// the round trip to happen to survive: a prompt carrying `\e[201~` has no
/// honest reading through this channel, and submitting it at all would mean the
/// remainder is being interpreted by a terminal rather than by pmux.
#[test]
fn a_prompt_containing_the_paste_terminator_is_always_refused() {
    for (name, prompt) in hostile_prompts() {
        if !prompt.contains("\x1b[201~") {
            continue;
        }
        let outcome = guard_chain(&prompt);
        assert!(
            matches!(outcome, Outcome::Refused(_)),
            "{name}: a prompt carrying the bracketed-paste terminator was SUBMITTED. \
             The terminal ends the paste at the terminator and reads the remainder as \
             keystrokes, into a composer one '/' away from /logout and /clear. \
             Outcome: {outcome:?}"
        );
    }
}

/// A caller may not type a slash command, however the solidus is dressed up.
///
/// This is the OTHER caller-controlled path to the command menu, and the
/// terminator tests above do not cover it: `/clear` contains no ESC, so it
/// survives both control-character guards and would be submitted *exactly* --
/// satisfying [`every_input_is_refused_or_submitted_exactly`] while handing
/// Claude Code a command. Submitting it faithfully is precisely the bug.
///
/// The invisible-prefix cases are not decoration. `validate_prompt` looks past
/// whitespace AND every Unicode format character (category Cf) before testing
/// for the solidus, because the reader on the other end is a Node/Ink TUI and
/// JS `String.prototype.trim` strips U+FEFF, which the White_Space property does
/// not contain. `"\u{feff}/clear"` would otherwise pass a `trim_start` check here
/// and arrive at a JS-side command detector as `/clear`.
#[test]
fn a_caller_can_never_type_a_slash_command() {
    for (name, prompt) in hostile_prompts() {
        // The guard's own rule: skip past whitespace and Unicode format
        // characters, then look for the solidus.
        let looks_like_a_command = prompt
            .trim_start_matches(|character: char| {
                character.is_whitespace() || matches!(character as u32, 0xad | 0x200b..=0x200f | 0x2060..=0x2064 | 0xfeff | 0xe0020..=0xe007f)
            })
            .starts_with('/');
        if !looks_like_a_command {
            continue;
        }
        assert!(
            matches!(guard_chain(&prompt), Outcome::Refused(_)),
            "{name}: a caller-supplied slash command was accepted. Submitting it \
             faithfully is the bug: Claude Code reads it as a command, not as a \
             prompt, and the menu it opens contains /logout, /exit and /config."
        );
    }
}

/// Both guards refuse independently.
///
/// `validate_prompt` is a policy filter and could be relaxed; the wire
/// precondition inside the backend must not be. Checking them separately is
/// what keeps a future relaxation of the first from silently opening the path,
/// because the second would still have to be changed on purpose.
#[test]
fn the_wire_encoder_refuses_escapes_even_without_the_service_filter() {
    for (name, prompt) in hostile_prompts() {
        if !prompt.contains('\u{1b}') && !prompt.contains('\0') {
            continue;
        }
        // Straight to the backend, bypassing `validate_prompt` entirely.
        assert!(
            bracketed_paste_payload(&prompt).is_err(),
            "{name}: the terminal backend accepted a prompt containing ESC or NUL \
             with the service filter bypassed"
        );
    }
}

/// Normalization folds line endings, composes canonically, and removes exactly
/// the trailing run the composer removes -- and changes no other TEXT at all.
///
/// "Submits exactly that text" is stated modulo `normalize_prompt`, so that
/// function is part of the contract. If it ever rewrote anything a reader would
/// see, the escape hatch would widen without any guard changing.
///
/// The second rule is stated as Unicode canonical equivalence rather than as
/// "and NFC", so it is a property of the result and not a copy of the
/// implementation: decomposing both sides erases the difference composition is
/// allowed to make and preserves every difference it is not. NFC entered this
/// contract because Claude Code was MEASURED to apply it to the prompt it
/// records, and a normalization that stopped one step short of what the
/// composer does is a turn that can never be acknowledged.
///
/// The third rule entered it the same way and for the same reason, and it is
/// the one that DOES delete text a reader would see: at 2.1.226 the composer
/// records a buffer with its trailing whitespace gone, so a prompt ending in a
/// space armed a turn whose recorded row could never equal it
/// (`docs/path-b-adversarial.md` sec. 11). It is stated here as a bound rather
/// than as a copy of `composer_submitted_text`: whatever normalization drops
/// must be a SUFFIX, and every character in it must be one the composer is
/// MEASURED to drop. A normalization that deleted an interior space, or a
/// trailing U+200B, fails this without the assertion having to know what the
/// rule is.
#[test]
fn normalization_folds_line_endings_and_composes_canonically_and_does_nothing_else() {
    use unicode_normalization::UnicodeNormalization;

    for (name, prompt) in hostile_prompts() {
        let normalized = normalize_prompt(&prompt);
        assert_eq!(
            normalize_prompt(&normalized),
            normalized,
            "{name}: normalization is not idempotent"
        );
        assert!(
            !normalized.contains('\r'),
            "{name}: a carriage return survived normalization"
        );
        let seen: String = normalized.replace('\n', "").nfd().collect();
        let sent: String = prompt.replace(['\r', '\n'], "").nfd().collect();
        let dropped = sent.strip_prefix(seen.as_str()).unwrap_or_else(|| {
            panic!("{name}: normalization changed text somewhere other than the end")
        });
        assert!(
            dropped.chars().all(is_trimmed_from_the_end),
            "{name}: normalization dropped {dropped:?}, which the composer does not drop"
        );
        // The bound above says nothing is dropped that should not be. This says
        // nothing is KEPT that will not survive, and it is the half that
        // protects a turn: a prompt still ending in a character the composer
        // removes is a prompt whose recorded row cannot equal the armed one, so
        // the instance dies proving it. Stated over the output rather than over
        // how the output was produced, so it holds for any future rule that
        // achieves it differently.
        assert!(
            !normalized.ends_with(is_trimmed_from_the_end),
            "{name}: normalization returned {normalized:?}, which still ends in a \
             character the composer removes"
        );
    }
}

/// **No character pmux refuses inside a prompt is deleted from its end.**
///
/// The one rule that decides where the trim may reach, stated over the guard
/// chain a caller actually meets rather than over either predicate alone. Two
/// sets were written separately -- what `normalize_prompt` deletes and what
/// `validate_prompt` refuses -- and where they overlapped, the delete ran
/// first and the refusal never fired. A caller who put one of those characters
/// inside a prompt was told; a caller who put the same character at the end had
/// it removed and got a different prompt answered, with nothing said.
///
/// It is one-directional on purpose. A character may be refused at the end and
/// admitted inside -- `\` is exactly that, MEASURED
/// (`docs/path-b-adversarial.md` sec. 11.1), because Enter reads the character
/// before the cursor and nothing else. The direction that has no defence is the
/// other one: pmux may not answer a prompt it would have refused had the same
/// character stood one place to the left.
///
/// **The domain is derived, not listed.** It is every character pmux's guards
/// can treat specially at all: `char::is_control` is what the service's
/// control-character clause reads, `is_ignorable_prompt_prefix` is whitespace
/// plus category Cf and therefore covers every character the trailing trim can
/// remove (`no_character_pmux_refuses_is_also_one_it_deletes` pins that
/// containment in the crate that owns both), and the three composer constants
/// name the rest. A character outside that union is ordinary text to every
/// guard here, in both positions.
///
/// The second assertion is what ties the two crates together. The trim
/// subtracts `pseudomux_claude::is_refused_wherever_it_stands`, and the service
/// writes its own control-character clause; that is two spellings again, and
/// this is the direction in which they may not diverge -- every character the
/// crate predicate names must really be refused by the daemon, or the
/// subtraction is protecting a prompt from a guard that no longer exists.
///
/// It is asked only of prompts that still CARRY the character once
/// `normalize_prompt` has run, and that condition is asked of `normalize_prompt`
/// rather than written down: `\r` is a control character this predicate names,
/// and no guard ever sees one, because the fold to `\n` happens in front of
/// them. Spelling `\r` into an exception here is how the exception outlives the
/// fold.
#[test]
fn a_character_refused_inside_a_prompt_is_refused_at_its_end_too() {
    let mut domain: std::collections::BTreeSet<char> = (0..=char::MAX as u32)
        .filter_map(char::from_u32)
        .filter(|character| {
            character.is_control() || pseudomux_claude::is_ignorable_prompt_prefix(*character)
        })
        .collect();
    domain.extend(pseudomux_claude::COMPOSER_MODE_PREFIXES);
    domain.extend(pseudomux_claude::COMPOSER_REWRITTEN_CHARACTERS);
    domain.insert(pseudomux_claude::COMPOSER_LINE_CONTINUATION);
    let mut deleted_after_being_refused = Vec::new();
    let mut survives_normalization = 0;
    for character in domain {
        let inside = format!("ask{character}me about it");
        let at_the_end = format!("ask me about it{character}");
        let interior = guard_chain(&inside);
        let trailing = guard_chain(&at_the_end);
        if let (Outcome::Refused(reason), Outcome::Wire(wire)) = (&interior, &trailing) {
            deleted_after_being_refused.push(format!(
                "U+{:04X} refused inside a prompt ({reason}) and delivered as {wire:?} at its end",
                character as u32
            ));
        }
        if pseudomux_claude::is_refused_wherever_it_stands(character) {
            for (prompt, outcome) in [(&inside, &interior), (&at_the_end, &trailing)] {
                if !normalize_prompt(prompt).contains(character) {
                    continue;
                }
                survives_normalization += 1;
                assert!(
                    matches!(outcome, Outcome::Refused(_)),
                    "U+{:04X} is subtracted from the trim as refused and the daemon takes \
                     {prompt:?}: {outcome:?}",
                    character as u32
                );
            }
        }
    }
    // The skip above is a filter and a filter can empty a set, which is how a
    // test comes to assert nothing at all.
    assert!(
        survives_normalization > 100,
        "only {survives_normalization} refused-character prompts survived normalization, \
         so this test is asking almost nothing"
    );
    assert!(
        deleted_after_being_refused.is_empty(),
        "{} character(s) whose treatment depends on where they stand:\n{}",
        deleted_after_being_refused.len(),
        deleted_after_being_refused.join("\n")
    );
}

/// The service's size limit is enforced on the NORMALIZED text, and one byte
/// past it is refused rather than truncated. A truncating limit would submit
/// text the caller did not write, which is the same class of failure as the
/// terminator.
#[test]
fn the_size_limit_refuses_rather_than_truncates() {
    let at_limit = "x".repeat(MAX_PROMPT_BYTES);
    assert!(
        matches!(guard_chain(&at_limit), Outcome::Wire(_)),
        "a prompt exactly at the limit must be accepted"
    );
    let past_limit = "x".repeat(MAX_PROMPT_BYTES + 1);
    assert!(
        matches!(guard_chain(&past_limit), Outcome::Refused(_)),
        "a prompt one byte past the limit must be refused"
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// The same requirement over generated inputs, so the corpus above is a
    /// floor and not a ceiling.
    ///
    /// The alphabet is weighted towards the bytes that matter: ESC, `[`, `~`,
    /// digits, `/`, CR and NUL are the ingredients of both a terminator and a
    /// slash command, so a random string over them reconstructs the dangerous
    /// shapes far more often than a random string over Unicode would.
    #[test]
    fn generated_inputs_are_refused_or_encoded_exactly(
        prompt in prop::collection::vec(
            prop_oneof![
                Just('\u{1b}'), Just('['), Just(']'), Just('~'), Just('/'),
                Just('2'), Just('0'), Just('1'), Just('\r'), Just('\n'),
                Just('\0'), Just('\t'), Just('a'), Just(' '), Just('\u{9b}'),
                Just('\u{276f}'), Just('\u{1f600}'), Just('\u{301}'),
            ],
            0..48,
        ).prop_map(|chars| chars.into_iter().collect::<String>()),
    ) {
        match guard_chain(&prompt) {
            Outcome::Refused(_) => {}
            Outcome::Wire(wire) => {
                let expected = normalize_prompt(&prompt);
                // An accepted prompt can carry no ESC at all, so the encoding is
                // unambiguous by construction: the only `\e[201~` in the wire
                // bytes is the one pmux appended.
                prop_assert!(
                    !expected.contains('\u{1b}'),
                    "accepted a prompt containing ESC: {:?}",
                    expected
                );
                prop_assert_eq!(
                    wire,
                    format!("\u{1b}[200~{expected}\u{1b}[201~"),
                    "the wire bytes are not the accepted prompt inside one paste"
                );
            }
        }
    }
}
