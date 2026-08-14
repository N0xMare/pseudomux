//! The one prompt-normalization rule every native pmux entry point applies to
//! text it read from argv, a file or stdin before handing it to the daemon.
//!
//! The composer-shape rule travels with it, re-exported rather than restated:
//! `bin/pmux` and `bin/pmux-mcp` link this crate and not the service, and the
//! `starts_with('/')` copy each of them used to carry is how the `!` bash-mode
//! prefix reached a composer for as long as it did.
//!
//! It lives here, in the crate both `pmux` and `claude-p` already link, because
//! it was written twice and the two copies drifted: `pmux` measured the
//! trailing-terminator death and fixed it, `claude-p` kept the half of the rule
//! it started with, and the facade's canonical `echo q | claude-p` invocation
//! armed a turn that could not be acknowledged for as long as the two disagreed.

/// Normalizes one prompt into the exact bytes a Claude composer can hold.
///
/// One rule: whatever [`pseudomux_claude::normalize_prompt`] is. That function
/// is called rather than restated because it is also what the daemon applies to
/// the typed prompt Claude records, and an armed prompt is compared against that
/// recorded one for equality: the two ends of that comparison have to be the
/// same rule, not two copies of it.
///
/// # It used to be two rules, and the second one was a measurement stated as a
/// special case
///
/// This function carried its own `strip_suffix('\n')`, dropping EXACTLY ONE
/// trailing newline -- the POSIX text-file terminator -- because arming a turn
/// with one guarantees `TranscriptError::UnexpectedTypedPrompt`: expected
/// `"ok\n"` can never equal actual `"ok"`, so `echo q | claude-p` died by
/// default. That was true and it was half the rule. The composer does not drop
/// one newline; it drops its whole trailing run of
/// [`pseudomux_claude::is_trimmed_from_the_end`] characters, MEASURED at 2.1.226
/// over spaces, `\n`, U+FEFF and U+3000 (`docs/path-b-adversarial.md` sec. 11).
///
/// That run is what the COMPOSER removes, less every character pmux refuses to
/// paste. A prompt ending in U+0009, U+000B, U+000C or U+0085 loses nothing
/// here and is refused by the caller's own guard instead, each with a message.
/// The first three are characters the composer records as something ELSE --
/// U+0009 as four spaces, measured mid-line, and U+000B and U+000C as `^K` and
/// `^L`, measured as a prompt's last character -- and U+0085 is one it records
/// VERBATIM (`docs/path-b-adversarial.md` sec. 12). Neither kind is a character
/// this function may quietly drop, which is why the rule subtracts both.
///
/// The sentence that stood here said the opposite in as many words -- *"a caller
/// who deliberately ends a prompt with a blank line still gets one"*. They did
/// not: the composer removed it, the recorded row differed from the armed
/// prompt, and the pooled instance was destroyed proving it. Two newlines were
/// enough, and so was one trailing space, which this rule never looked at.
///
/// So the rule moved to `normalize_prompt`, where the other two transformations
/// already live and where BOTH ends of the equality apply it, and this function
/// is what is left: the one name `pmux`, `pmux-mcp` and `claude-p` call, kept so
/// that a caller reading it is told which rule applies rather than having to
/// know that two of them agree today. Nothing else is trimmed, added,
/// re-wrapped or re-encoded; a caller that needs a bound, an emptiness check or
/// a composer guard applies its own, over this result.
pub use pseudomux_claude::{
    COMPOSER_LINE_CONTINUATION, COMPOSER_MODE_PREFIXES, COMPOSER_REWRITTEN_CHARACTERS,
    ComposerRefusal, composer_refusal, composer_submitted_text, is_trimmed_from_the_end,
};

#[must_use]
pub fn normalize_cli_prompt(prompt: &str) -> String {
    pseudomux_claude::normalize_prompt(prompt)
}
