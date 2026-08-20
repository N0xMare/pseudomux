use std::ffi::OsStr;
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use pseudomux_protocol::v1::EffortLevel;

pub const MAX_PROMPT_BYTES: u64 = 1024 * 1024;
const MAX_PROMPT_SOURCE_BYTES: u64 = MAX_PROMPT_BYTES * 2 + 1;

#[derive(Parser, Debug)]
#[command(
    name = "pmux",
    version,
    about = "Thin CLI for the pmux token engine",
    long_about = "pmux talks to a local pool of embedded Claude Code processes.

`run` is the one-shot CLI: `(model, effort, prompt) -> text + usage`. The
caller names no resource. `pmuxd` must have been started with --pool-parent
or every `run` is refused. `ping` and `doctor` start nothing and spend no
tokens.

Harnesses should use the Messages facade (`--messages-bind`)
with `x-pmux-conversation`, not this CLI."
)]
pub struct Cli {
    /// Exact pmuxd Unix socket. No discovery or daemon startup is performed.
    #[arg(long, env = "PMUX_SOCKET")]
    pub socket: PathBuf,

    /// Output representation. `json` is one object; `ndjson` is one
    /// `{"type","data"}` record per line. Every published subcommand emits
    /// exactly one record in either mode.
    #[arg(long, value_enum, default_value_t, global = true)]
    pub output: OutputMode,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum OutputMode {
    #[default]
    Text,
    Json,
    Ndjson,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Ops: ask the daemon for its version and protocol number.
    ///
    /// Starts nothing, spends no tokens, and reaches only the accept loop. Use
    /// `pmux doctor` for anything behind it.
    Ping,
    /// API: run one stateless turn against the embedded Claude Code pool.
    ///
    /// Requires a `pmuxd` started with `--pool-parent`; without one every
    /// `run` is refused with `unsupported_feature`.
    ///
    /// THE CALLER NAMES NO RESOURCE. There is no `--cwd`, no
    /// `--config-isolation-root`, no `--claude`, no `--system-prompt`, no
    /// session id and no generation on this subcommand, and their absence is
    /// the product rather than an omission: the daemon mints every one of them
    /// from its own configuration plus a slot identity.
    ///
    /// `(model, effort, prompt) -> text + usage`, and nothing else.
    #[command(alias = "ask")]
    Run {
        /// Claude model alias or exact id, e.g. `opus`, `sonnet`,
        /// `claude-opus-5`. Required: it is half the pool's class key, and an
        /// absent model would partition the pool on whatever the daemon's
        /// configuration happens to default to.
        #[arg(long)]
        model: String,
        /// Reasoning depth. Omit for the resolved model's own default.
        /// Validated against the RESOLVED model by the daemon, never against
        /// this list alone -- tiers are not uniform across Claude models.
        #[arg(long, value_enum)]
        effort: Option<EffortArg>,
        #[command(flatten)]
        prompt: PromptArgs,
        /// Absolute wall-clock deadline for the answer. Omit for daemon policy.
        /// It may only SHORTEN pmux's wait; nothing here lengthens one.
        #[arg(long)]
        deadline_unix_ms: Option<u64>,
    },
    /// Ops: validate the socket, the daemon's health tree, and the Claude
    /// executable.
    ///
    /// Starts no session and spends no tokens. Exits 0 only when every check it
    /// lists both ran and passed; `unproven` and `unhealthy` both exit 1, and
    /// the `status` field is the distinction.
    ///
    /// The health tree includes the daemon's own compatibility layer, which
    /// runs the Claude the stateless pool would launch and asks the same
    /// registry a mint asks. That is what stops a green `doctor` from being
    /// followed by a `run` refused with `unsupported_claude_version`: the two
    /// answers now come from one comparison, made where both operands live.
    Doctor {
        /// Claude executable to validate. This is not the pool's `--pool-claude`.
        ///
        /// Doctor checks this binary under `AllowUntested`, so an unmeasured
        /// version is not a fault here; the version gate is `RequireTested`
        /// and applies to the pool's own executable, which the daemon reports
        /// on in the health tree above rather than this client guessing at it.
        #[arg(long, env = "PMUX_CLAUDE", default_value = "claude")]
        claude: PathBuf,
    },
}

#[derive(Clone, Debug, Args)]
pub struct PromptArgs {
    /// Prompt text. If omitted, stdin is read when it is not a terminal.
    #[arg(value_name = "PROMPT", conflicts_with = "prompt_file")]
    pub prompt: Option<String>,
    /// Read the prompt from this UTF-8 file; `-` means stdin. One trailing
    /// newline is dropped, so an ordinary text file works unchanged.
    #[arg(long, conflicts_with = "prompt")]
    pub prompt_file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum EffortArg {
    Low,
    Medium,
    High,
    #[value(name = "xhigh")]
    XHigh,
    Max,
}

pub fn read_prompt(args: &PromptArgs) -> Result<String> {
    let bytes = match (&args.prompt, &args.prompt_file) {
        (Some(prompt), None) => prompt.as_bytes().to_vec(),
        (None, Some(path)) if path == Path::new("-") => read_limited(io::stdin().lock())?,
        (None, Some(path)) => {
            let file = fs::File::open(path)
                .with_context(|| format!("failed to open prompt file {}", path.display()))?;
            read_limited(file)?
        }
        (None, None) if !io::stdin().is_terminal() => read_limited(io::stdin().lock())?,
        (None, None) => bail!("provide PROMPT, --prompt-file, or pipe UTF-8 prompt text on stdin"),
        (Some(_), Some(_)) => bail!("prompt sources are mutually exclusive"),
    };
    let prompt = String::from_utf8(bytes).context("prompt must be valid UTF-8")?;
    // Line endings folded, NFC, and the composer's WHOLE trailing trim applied
    // -- not one terminator, all of them, plus every other character
    // `pseudomux_claude::is_trimmed_from_the_end` names.
    //
    // This comment said "exactly one text-file terminator dropped" and called
    // it "the same `exactly one` rule `crates/claude/src/cursor.rs:196` applies
    // to a trailing CR". Both halves stopped being true in `48aee00`: the
    // one-newline rule was measured to be half of what the composer does (two
    // newlines, and one trailing space, each destroyed the instance that proved
    // it), and `crates/client/src/prompt.rs` now carries the retraction at
    // length. The cursor's CR rule really is exactly-one, about a different
    // boundary, and the equivalence is what would invite the `strip_suffix`
    // back.
    //
    // The rule and the measurement behind it live in `normalize_cli_prompt`.
    // A second copy of that function is how a previous facade kept the
    // terminator that killed every turn it armed.
    let normalized = pseudomux_client::normalize_cli_prompt(&prompt);
    if normalized.len() as u64 > MAX_PROMPT_BYTES {
        bail!("prompt exceeds the {MAX_PROMPT_BYTES}-byte CLI limit");
    }
    if normalized.is_empty() {
        bail!("prompt must not be empty");
    }
    if let Some(refusal) = pseudomux_client::prompt::composer_refusal(&normalized) {
        bail!("{}", refusal.describe());
    }
    // `\t` is deliberately absent from this exception where it once stood: the
    // composer rule above has already refused it, with a message that says
    // what the composer does to it.
    for character in normalized.chars() {
        if character == '\0'
            || character == '\u{1b}'
            || (character.is_control() && !matches!(character, '\n' | '\t'))
        {
            bail!("prompt contains an unsafe control character");
        }
    }
    Ok(normalized)
}

/// Doctor's `--claude` clap default, and the fallback when none is named.
const DEFAULT_CLAUDE_EXECUTABLE: &str = "claude";

pub fn resolve_executable(requested: Option<&Path>) -> Result<PathBuf> {
    let requested = requested.unwrap_or_else(|| Path::new(DEFAULT_CLAUDE_EXECUTABLE));
    let candidate = if requested.is_absolute() {
        requested.to_path_buf()
    } else if requested.components().count() > 1 {
        std::env::current_dir()?.join(requested)
    } else {
        find_on_path(requested.as_os_str()).with_context(|| {
            format!(
                "Claude executable not found on PATH: {}",
                requested.display()
            )
        })?
    };
    let resolved = fs::canonicalize(&candidate)
        .with_context(|| format!("Claude executable does not exist: {}", candidate.display()))?;
    let metadata = fs::metadata(&resolved)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        bail!(
            "Claude path is not an executable file: {}",
            resolved.display()
        );
    }
    Ok(resolved)
}

fn find_on_path(executable: &OsStr) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(executable))
        .find(|candidate| {
            fs::metadata(candidate)
                .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

fn read_limited(reader: impl Read) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_PROMPT_SOURCE_BYTES)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

impl From<EffortArg> for EffortLevel {
    fn from(value: EffortArg) -> Self {
        match value {
            EffortArg::Low => Self::Low,
            EffortArg::Medium => Self::Medium,
            EffortArg::High => Self::High,
            EffortArg::XHigh => Self::XHigh,
            EffortArg::Max => Self::Max,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::CommandFactory;

    use super::*;

    /// The rendered `--help` for one subcommand, exactly as a user sees it.
    ///
    /// `build()` first: clap propagates `global = true` arguments into every
    /// subcommand during the build, so a help rendered off an unbuilt command
    /// is missing exactly the arguments this file made global.
    fn rendered_help(subcommand: &str) -> String {
        let mut command = Cli::command();
        command.build();
        command
            .find_subcommand_mut(subcommand)
            .unwrap_or_else(|| panic!("no {subcommand} subcommand"))
            .render_long_help()
            .to_string()
    }

    /// Every subcommand a user can type, `help` excluded: it is clap's and its
    /// text is not ours to hold to these rules.
    fn user_subcommands() -> Vec<clap::Command> {
        Cli::command()
            .get_subcommands()
            .filter(|command| command.get_name() != "help")
            .cloned()
            .collect()
    }

    #[test]
    fn requires_explicit_socket() {
        let result = Cli::try_parse_from(["pmux", "ping"]);
        assert!(result.is_err());
    }

    /// NOTHING TESTED HELP TEXT BEFORE THIS FILE'S POLISH PASS, and five
    /// subcommands plus twenty-three arguments shipped with none at all --
    /// `ping`, `inspect`, `cancel`, `close` and `attach` had no description in
    /// `pmux --help`, and `--model`, `--effort`, `--rows`, `--read-only`,
    /// `--launch`, `--keep` and the rest rendered as a bare flag name.
    ///
    /// The set walked here is clap's own command tree, so a flag added later
    /// without help is red without anyone remembering to list it. That is the
    /// whole point: a hand-kept inventory of "arguments that must have help" is
    /// the defect this repo keeps finding, one level up.
    #[test]
    fn every_subcommand_and_argument_a_user_can_type_carries_help_text() {
        let mut missing = Vec::new();
        for command in user_subcommands() {
            if command.get_about().is_none() {
                missing.push(command.get_name().to_owned());
            }
            for argument in command.get_arguments() {
                // `-h/--help` is clap's own and is documented by clap.
                if argument.get_id() == "help" {
                    continue;
                }
                if argument.get_help().is_none() && argument.get_long_help().is_none() {
                    missing.push(format!("{} {}", command.get_name(), argument.get_id()));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "these subcommands/arguments render in --help with no description: {missing:?}"
        );
    }

    /// A user must be able to tell, from `pmux --help` alone, which product
    /// they are driving.
    ///
    /// Derived over every subcommand rather than over a list, so a subcommand
    /// added later has to answer the question too. The published surface is
    /// only ping/run/doctor, labelled API or Ops.
    #[test]
    fn every_subcommand_says_which_surface_it_is_on() {
        let mut names = Vec::new();
        for command in user_subcommands() {
            let name = command.get_name().to_owned();
            names.push(name.clone());
            let about = command
                .get_about()
                .expect("about is required above")
                .to_string();
            let expected = match name.as_str() {
                "ping" | "doctor" => "Ops:",
                "run" => "API:",
                other => panic!("published surface is ping/run/doctor; found {other}"),
            };
            assert!(
                about.starts_with(expected),
                "`pmux {name}` is not labelled {expected:?}: {about:?}"
            );
        }
        names.sort();
        assert_eq!(names, ["doctor", "ping", "run"]);
    }

    /// The provider CLI's product statement, held on the CLI the way
    /// `the_run_stateless_tool_refuses_every_resource_a_caller_might_name`
    /// holds it on the MCP schema.
    ///
    /// Derived from clap's own argument ids for `run`, so a resource flag added
    /// to this subcommand later is red here rather than in a leak report.
    #[test]
    fn the_run_subcommand_names_no_resource() {
        let command = Cli::command().find_subcommand_mut("run").unwrap().clone();
        let offered: BTreeSet<String> = command
            .get_arguments()
            .map(|argument| argument.get_id().to_string())
            .collect();
        // Every argument `run` declares of its own. `--socket` and `--output`
        // are global and belong to the binary rather than to this subcommand;
        // neither names a resource the daemon would use for the turn.
        let admitted = BTreeSet::from([
            "model".to_owned(),
            "effort".to_owned(),
            "prompt".to_owned(),
            "prompt_file".to_owned(),
            "deadline_unix_ms".to_owned(),
        ]);
        assert_eq!(
            offered, admitted,
            "`pmux run` gained or lost an argument; every addition here is a resource a \
             caller could name, which is the one thing this subcommand promises it cannot do"
        );
    }

    /// EVERYWHERE a prompt is taken, it is takeable from a file.
    ///
    /// The rule is derived from the argument names rather than stated over a
    /// list: any argument whose id mentions a prompt and is not itself a file
    /// form must have a `<id>_file` sibling in the same subcommand. A future
    /// `--review-prompt` is therefore held to the same rule without anyone
    /// remembering this test exists.
    #[test]
    fn every_prompt_this_cli_takes_is_also_takeable_from_a_file() {
        let mut argv_only = Vec::new();
        for command in user_subcommands() {
            let ids: BTreeSet<String> = command
                .get_arguments()
                .map(|argument| argument.get_id().to_string())
                .collect();
            for id in &ids {
                if !id.contains("prompt") || id.ends_with("_file") {
                    continue;
                }
                if !ids.contains(&format!("{id}_file")) {
                    argv_only.push(format!("{} {id}", command.get_name()));
                }
            }
        }
        assert!(
            argv_only.is_empty(),
            "these prompts can only be given on argv, where `ps` can read them: {argv_only:?}"
        );
    }

    /// `--output` is `global = true`, so its one help string is rendered on
    /// every subcommand. It used to read "NDJSON includes turn events followed
    /// by a result record" under `pmux ping`.
    #[test]
    fn the_global_output_help_does_not_promise_turn_events_to_subcommands_that_have_none() {
        for subcommand in ["ping", "run", "doctor"] {
            let help = rendered_help(subcommand);
            assert!(
                help.contains("exactly one record"),
                "`pmux {subcommand} --help` does not scope the --output description: {help}"
            );
            assert!(
                !help.to_lowercase().contains("turn events"),
                "`pmux {subcommand} --help` still promises turn events: {help}"
            );
        }
    }

    #[test]
    fn prompt_normalizes_and_rejects_terminal_controls() {
        let prompt = read_prompt(&PromptArgs {
            prompt: Some("one\r\ntwo\rthree".into()),
            prompt_file: None,
        })
        .unwrap();
        assert_eq!(prompt, "one\ntwo\nthree");

        let error = read_prompt(&PromptArgs {
            prompt: Some("unsafe\u{1b}[2J".into()),
            prompt_file: None,
        })
        .unwrap_err();
        assert!(error.to_string().contains("unsafe control"));
    }

    #[test]
    fn no_cli_prompt_can_carry_a_composer_mode_command_to_the_daemon() {
        // The rule is `pseudomux_client::prompt::composer_refusal`, which the
        // service's `validate_prompt` also calls; the CLI refusal exists so a
        // caller learns before a turn is submitted. It used to be a second
        // statement of one intention, and the two drifted in the only way that
        // mattered: both named `/` and neither named `!`.
        let read = |text: &str| {
            read_prompt(&PromptArgs {
                prompt: Some(text.into()),
                prompt_file: None,
            })
        };
        // Derived from the shipped mode set, so a character added to it adds
        // its own 22 cases rather than being tested by nobody.
        let invisibles = [
            "",
            " ",
            "\t",
            "\n",
            "\r\n",
            "\r",
            "  \t\n  ",
            "\u{a0}",              // NO-BREAK SPACE
            "\u{85}",              // NEXT LINE
            "\u{2003}",            // EM SPACE
            "\u{202f}",            // NARROW NO-BREAK SPACE
            "\u{3000}",            // IDEOGRAPHIC SPACE
            "\u{feff}",            // ZERO WIDTH NO-BREAK SPACE: stripped by JS `trim`
            "\u{200b}",            // ZERO WIDTH SPACE
            "\u{2060}",            // WORD JOINER
            "\u{ad}",              // SOFT HYPHEN
            "\u{200e}",            // LEFT-TO-RIGHT MARK
            "\u{202e}",            // RIGHT-TO-LEFT OVERRIDE
            "\u{feff} \u{200b}\t", // invisibles and whitespace interleaved
        ];
        let mut attempts = Vec::new();
        for prefix in pseudomux_client::prompt::COMPOSER_MODE_PREFIXES {
            for invisible in invisibles {
                attempts.push(format!("{invisible}{prefix}payload"));
            }
            attempts.push(format!("{prefix}payload\n"));
            attempts.push(format!("{prefix}payload\nand more"));
            attempts.push(format!("{prefix}{prefix}payload"));
            attempts.push(prefix.to_string());
        }
        for attempt in &attempts {
            let attempt = attempt.as_str();
            let error = read(attempt)
                .expect_err(&format!("prompt {attempt:?} must be refused"))
                .to_string();
            assert!(
                error.contains("switches the composer") || error.contains("command menu"),
                "prompt {attempt:?} was refused for the wrong reason: {error}"
            );
        }

        // Shapes that are not slash commands and must keep working. No reading
        // of these puts U+002F in first position -- not Rust's `trim_start`,
        // not JS's `trim`, not the invisible-format rule the guard applies --
        // and refusing them would break ordinary prompts (a pasted path, a
        // quoted command, a lookalike glyph, text carried out of a
        // Windows-authored file) for a threat that does not exist.
        for attempt in [
            "\u{2044}clear",        // FRACTION SLASH
            "\u{2215}clear",        // DIVISION SLASH
            "\u{ff0f}clear",        // FULLWIDTH SOLIDUS
            "\u{29f8}clear",        // BIG SOLIDUS
            "\u{feff}explain this", // a BOM ahead of ordinary text is ordinary text
            "\u{200b}explain this",
            "explain this:\n/clear",
            "explain this:\r\n/clear",
            "src/main.rs",
        ] {
            let prompt = read(attempt)
                .unwrap_or_else(|error| panic!("prompt {attempt:?} was refused: {error}"));
            assert_eq!(
                pseudomux_client::prompt::composer_refusal(&prompt),
                None,
                "prompt {attempt:?} would reach the composer as a mode character"
            );
            // The guard reads past those characters without removing them: the
            // daemon must receive the caller's bytes, since they are the text
            // the typed-prompt acknowledgement is matched against. Only the
            // line-ending normalization every prompt gets is expected here.
            assert_eq!(
                prompt,
                attempt.replace("\r\n", "\n"),
                "prompt {attempt:?} was rewritten on its way to the daemon"
            );
        }
    }

    #[test]
    fn prompt_drops_the_whole_trailing_run_the_composer_drops() {
        // Every conventional tool terminates a text file with a newline, but a
        // composer cannot hold one, so Claude records the typed prompt without
        // it. Keeping it made `expected` unequal to `actual` at engine.rs:127
        // and every --prompt-file turn died in `UnexpectedTypedPrompt`.
        //
        // This test asserted that EXACTLY ONE newline was dropped until
        // 2026-08-09, and named that as the guarantee that "a deliberate
        // trailing blank line survives". It did not survive: at 2.1.226 the
        // composer removes its whole trailing run of whitespace, so `"poem\n\n"`
        // was typed as `"poem\n"`, recorded as `"poem"`, and cost the pooled
        // instance -- as did `"  padded  "`, whose two trailing spaces this
        // rule never looked at (`docs/path-b-adversarial.md` sec. 11).
        let read = |text: &str| {
            read_prompt(&PromptArgs {
                prompt: Some(text.into()),
                prompt_file: None,
            })
            .unwrap()
        };
        assert_eq!(read("Reply with exactly: ok\n"), "Reply with exactly: ok");
        assert_eq!(read("Reply with exactly: ok"), "Reply with exactly: ok");
        assert_eq!(read("poem\n\n"), "poem");
        // CRLF is folded first, so a CRLF-terminated file behaves identically.
        assert_eq!(read("line one\r\nline two\r\n"), "line one\nline two");
        // Trailing whitespace goes; LEADING and INTERIOR whitespace stay. This
        // is the composer's `trimEnd` and not a `trim`, MEASURED both ways.
        assert_eq!(read("  padded  \n"), "  padded");
        assert_eq!(read("line one   \nline two"), "line one   \nline two");
        // The one invisible the composer keeps, so pmux keeps it too.
        assert_eq!(read("ok\u{200b}"), "ok\u{200b}");
        // Nothing but whitespace is nothing: refused as empty rather than typed
        // into a composer whose Enter would do nothing at all.
        let empty = read_prompt(&PromptArgs {
            prompt: Some("   \n ".into()),
            prompt_file: None,
        })
        .expect_err("a whitespace-only prompt is empty");
        assert!(
            empty.to_string().contains("must not be empty"),
            "got {empty}"
        );
    }

    #[test]
    fn the_terminator_is_not_charged_against_the_prompt_byte_budget() {
        // The strip runs before the length check, so a source carrying the full
        // budget of content plus its terminator is accepted and delivers the
        // full budget. Charging the terminator would make a file of exactly
        // MAX_PROMPT_BYTES of content unusable for a reason the caller cannot
        // see in its own content.
        let limit = usize::try_from(MAX_PROMPT_BYTES).unwrap();
        let at_limit_plus_terminator = "x".repeat(limit) + "\n";
        let prompt = read_prompt(&PromptArgs {
            prompt: Some(at_limit_plus_terminator),
            prompt_file: None,
        })
        .unwrap();
        assert_eq!(prompt.len(), limit);

        // One byte of real content past the budget is still refused.
        let over = "x".repeat(limit + 1) + "\n";
        let error = read_prompt(&PromptArgs {
            prompt: Some(over),
            prompt_file: None,
        })
        .unwrap_err();
        assert!(error.to_string().contains("exceeds"), "{error}");
    }

    #[test]
    fn run_alias_and_output_mode_parse() {
        let cli = Cli::try_parse_from([
            "pmux",
            "--socket",
            "/tmp/pmux.sock",
            "--output",
            "ndjson",
            "ask",
            "--model",
            "sonnet",
            "hello",
        ])
        .unwrap();
        assert_eq!(cli.output, OutputMode::Ndjson);
        assert!(matches!(cli.command, Command::Run { .. }));
    }
}
