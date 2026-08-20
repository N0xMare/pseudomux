//! Operator configuration for the stateless pool, refused at boot.
//!
//! Which of these are operator-configurable: **all of them**. Which are
//! caller-configurable: **none of them**. That is the product statement, not an
//! aesthetic -- nine leaks in this codebase were each reachable only because a
//! caller could name a resource pmux also used, and a caller who cannot name a
//! resource cannot alias one.
//!
//! Everything here is validated once, at boot, by [`PoolSettings::validate`].
//! A refusal at boot beats a refusal at turn 200, and it is not a runtime
//! branch.

use std::path::{Path, PathBuf};

use pseudomux_protocol::v1::EffortLevel;

use super::class::{resolve_pool_class, InstanceClass, ModelEffortRefusal};

/// Owner-set upper limit on live instances. `--pool-size` is refused
/// above this at parse, so the runtime never has to consider a larger pool.
pub const MAX_POOL_SIZE: u32 = 15;
/// Owner-set default, equal to the limit.
pub const DEFAULT_POOL_SIZE: u32 = 15;

/// Owner-set default number of turns one instance serves before it is recycled.
pub const DEFAULT_RECYCLE_TURNS: u32 = 50;
/// Ceiling so the knob cannot be turned into "never recycle".
pub const MAX_RECYCLE_TURNS: u32 = 250;

/// The system prompt bound, enforced in bytes and refused at boot.
///
/// Deliberately a byte bound and not a sentence counter. A sentence counter
/// rejects a correct prompt containing "e.g." -- it is a rule pretending to be
/// a proof. 512 bytes is what the daemon enforces.
pub const MAX_SYSTEM_PROMPT_BYTES: usize = 512;

/// Displaces Claude Code's default agent prompt. Not consumer policy.
/// Empty is refused; omitting REPLACE restores Claude Code's default.
pub const DEFAULT_SYSTEM_PROMPT: &str = "The user message is the entire instruction.";

/// CHOSEN: five minutes. Long enough that a bursty caller keeps its warm
/// class, short enough that a cold class returns its slot within one coffee.
pub const DEFAULT_INSTANCE_IDLE_TTL_MS: u64 = 300_000;

/// The ceiling on how long admission waits for a slot that is coming back.
///
/// **CHOSEN against the measurement, and the first number chosen for it was
/// wrong**, which is the part worth keeping on the record.
///
/// It was first written as 500 ms, on the strength of the "~30 ms" that
/// `docs/path-b.md` sec.3.4 measures and that every comment about `Clearing` in
/// this crate had inherited. **That number is about a different thing.** It is
/// the rotation: the interval from Enter to the new transcript file existing,
/// measured at the file. What `Pool::finish_turn` awaits is
/// [`super::host::InstanceHost::clear`] end to end -- `/clear` driven into the
/// composer, the local-command menu resolved, and then the rebound transcript
/// PROVEN inert, a proof that carries the compatibility profile's
/// `transcript_drain_ms`. MEASURED over the socket, all seven clears of one
/// 8-concurrent-caller wave against 3 slots at the test double's 50 ms drain:
/// **703, 723, 727, 730, 748, 749, 756 ms**, median 730.
///
/// At a 500 ms ceiling that same wave served 7 of 24 callers and printed
/// refusals reading "1 clearing between turns, with no caller waiting" about
/// 230 ms before that clear finished. A ceiling under the thing it is waiting
/// for is a slower spelling of the defect it was added to fix.
///
/// So the number is the housekeeping cycle plus margin: ~750 ms at the double's
/// 50 ms drain, ~1700 ms at the 1000 ms drain the promoted 2.1.220 profile
/// ships (`compatibility::PROMOTED_PROFILES`), and 2500 ms is above that with
/// half again on top. A teardown -- a close, a positive reaping and one
/// `rmtree` -- is the other thing waited on and is bounded by the same number.
///
/// It is a HARD bound and not a target. The PREDICATE
/// ([`super::machine::CensusBucket::comes_back_on_its_own`]) is what keeps a
/// caller from ever waiting on a model's turn; this exists because a pool under
/// sustained load always has something clearing, so the predicate alone would
/// let one caller wait forever. The caller's own deadline bounds it further, and
/// the smaller of the two wins -- which is how a caller that wants a faster
/// answer than this asks for one.
pub const ADMISSION_WAIT_CEILING_MS: u64 = 2_500;

/// How long the admission wait sleeps between re-reads of the pool.
///
/// A poll rather than a condition variable, deliberately. A notification-based
/// wait is only as live as the set of sites that remember to signal it, and a
/// hand-maintained set of sites is the exact defect class this module keeps
/// finding: the signal would have to be placed at every mutation that can
/// improve availability, and a missed one is a caller asleep beside a free slot.
/// A re-read of the pool's own state cannot be wrong about what the pool holds.
///
/// The cost of that choice is bounded by this number and paid only by a caller
/// that would otherwise have been REFUSED: 5 ms against a MEASURED 703-756 ms
/// clear is under a hundredth of the thing being waited for, and a caller that
/// is not waiting never reads it. [`ADMISSION_WAIT_CEILING_MS`] divided by this
/// is the most re-reads one waiting caller can perform -- 500, each one a lock,
/// two map reads and a release.
pub const ADMISSION_POLL_MS: u64 = 5;

/// DERIVED from the measured `375 + 1.86n` MB growth: `375 + 50 * 1.86 = 468`
/// MB expected per instance at a 50-turn recycle, against a 1024 MB per-instance
/// ceiling that the turn cap makes arithmetically unreachable
/// (`(1024 - 375) / 1.86 = 349` turns). The ceiling therefore gates nothing at
/// runtime; it is a boot assertion about how the host was sized.
pub const RSS_CEILING_MB_PER_INSTANCE: u64 = 1024;

/// One operator-declared warm class and how many instances to hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WarmClassSetting {
    pub model: String,
    pub effort: Option<EffortLevel>,
    pub count: u32,
}

/// A warm class after resolution: the class key, and the floor to hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WarmClass {
    pub class: InstanceClass,
    pub count: u32,
}

/// Operator input, exactly as parsed. Nothing here is trusted yet.
#[derive(Clone, Debug)]
pub struct PoolSettings {
    pub pool_size: u32,
    pub recycle_turns: u32,
    pub system_prompt: String,
    pub instance_idle_ttl_ms: u64,
    pub turn_timeout_ms: u64,
    pub parent_dir: PathBuf,
    pub claude_executable: PathBuf,
    pub retain_dir: Option<PathBuf>,
    /// Where the redacted Path B evidence corpus is written, or `None` to
    /// retain nothing.
    ///
    /// ON BY DEFAULT, because the thing it buys cannot be bought later: at a
    /// new Claude Code version there are no `cli` turns to re-analyse
    /// (`docs/version-drift.md` sec.2.1), so a corpus that starts accumulating
    /// when a promotion is needed starts empty. `crate::pool::evidence` is what
    /// it holds and why it holds so little of it.
    pub evidence_dir: Option<PathBuf>,
    pub warm_set: Vec<WarmClassSetting>,
    pub rss_budget_mb: u64,
}

impl PoolSettings {
    /// The operator defaults, given the two paths that have no default.
    #[must_use]
    pub fn defaults(parent_dir: PathBuf, claude_executable: PathBuf) -> Self {
        Self {
            pool_size: DEFAULT_POOL_SIZE,
            recycle_turns: DEFAULT_RECYCLE_TURNS,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_owned(),
            instance_idle_ttl_ms: DEFAULT_INSTANCE_IDLE_TTL_MS,
            turn_timeout_ms: 600_000,
            parent_dir,
            claude_executable,
            retain_dir: None,
            // `None` HERE and on by default at the daemon: this constructor
            // takes only the two paths that have no default, and the evidence
            // directory is derived from `--socket` exactly as `logs/` and
            // `pool-evidence/` are. A default invented here would be a second
            // derivation, in a type that cannot see the socket.
            evidence_dir: None,
            warm_set: Vec::new(),
            rss_budget_mb: u64::from(DEFAULT_POOL_SIZE) * RSS_CEILING_MB_PER_INSTANCE,
        }
    }

    /// Refuses at boot, or hands back a configuration nothing downstream has to
    /// re-check.
    ///
    /// # Errors
    ///
    /// Returns the first violated bound. There is deliberately no "warn and
    /// continue" arm: a pool that boots on an inadmissible constant is a pool
    /// whose guarantees were negotiated by a log line.
    pub fn validate(self) -> Result<PoolConfig, ConfigRefusal> {
        if self.pool_size == 0 || self.pool_size > MAX_POOL_SIZE {
            return Err(ConfigRefusal::PoolSizeOutOfRange {
                requested: self.pool_size,
                maximum: MAX_POOL_SIZE,
            });
        }
        if self.recycle_turns == 0 || self.recycle_turns > MAX_RECYCLE_TURNS {
            return Err(ConfigRefusal::RecycleTurnsOutOfRange {
                requested: self.recycle_turns,
                maximum: MAX_RECYCLE_TURNS,
            });
        }
        validate_system_prompt(&self.system_prompt)?;
        require_absolute(&self.parent_dir, ConfigField::ParentDir)?;
        require_absolute(&self.claude_executable, ConfigField::ClaudeExecutable)?;
        // ONE rule, applied to both directories that must outlive the tree
        // they are taken from. It was written once for `--path-b-retain-dir`
        // and the reason it gives -- "evidence must outlive the tree it is
        // taken from" -- is exactly as true of the evidence corpus, which is
        // written from a config root the next line erases.
        for (field, directory) in [
            (ConfigField::RetainDir, &self.retain_dir),
            (ConfigField::EvidenceDir, &self.evidence_dir),
        ] {
            let Some(directory) = directory else {
                continue;
            };
            require_absolute(directory, field)?;
            if directory.starts_with(&self.parent_dir) {
                return Err(ConfigRefusal::DirInsidePoolParent {
                    field,
                    directory: directory.clone(),
                    parent_dir: self.parent_dir.clone(),
                });
            }
        }
        if self.instance_idle_ttl_ms == 0 {
            return Err(ConfigRefusal::ZeroIdleTtl);
        }
        if self.turn_timeout_ms == 0 {
            return Err(ConfigRefusal::ZeroTurnTimeout);
        }

        // The boot assertion that replaces a runtime RSS predicate. A per-pid
        // sampler, a sampling interval and a platform abstraction bought a
        // bound the turn cap already enforces; two lines of arithmetic here
        // buy the same thing without a branch that can never be taken.
        let required_mb = u64::from(self.pool_size) * RSS_CEILING_MB_PER_INSTANCE;
        if required_mb > self.rss_budget_mb {
            return Err(ConfigRefusal::RssBudgetTooSmall {
                pool_size: self.pool_size,
                required_mb,
                budget_mb: self.rss_budget_mb,
            });
        }

        let warm_set = resolve_warm_set(&self.warm_set, self.pool_size)?;

        Ok(PoolConfig {
            pool_size: self.pool_size,
            recycle_turns: self.recycle_turns,
            system_prompt_fingerprint: fingerprint(&self.system_prompt),
            system_prompt: self.system_prompt,
            instance_idle_ttl_ms: self.instance_idle_ttl_ms,
            turn_timeout_ms: self.turn_timeout_ms,
            parent_dir: self.parent_dir,
            claude_executable: self.claude_executable,
            retain_dir: self.retain_dir,
            evidence_dir: self.evidence_dir,
            warm_set,
        })
    }
}

fn resolve_warm_set(
    declared: &[WarmClassSetting],
    pool_size: u32,
) -> Result<Vec<WarmClass>, ConfigRefusal> {
    let mut resolved: Vec<WarmClass> = Vec::with_capacity(declared.len());
    let mut total: u32 = 0;
    for setting in declared {
        if setting.count == 0 {
            return Err(ConfigRefusal::ZeroWarmCount {
                model: setting.model.clone(),
            });
        }
        // Resolved through the SAME call the pool uses for a live request, so a
        // warm class that the pool could never serve is refused at boot rather
        // than discovered by an operator reading a mint failure.
        let (class, _) = resolve_pool_class(&setting.model, setting.effort).map_err(|refusal| {
            ConfigRefusal::WarmClassNotAdmitted {
                model: setting.model.clone(),
                refusal,
            }
        })?;
        if resolved.iter().any(|warm| warm.class == class) {
            return Err(ConfigRefusal::DuplicateWarmClass {
                class: class.to_string(),
            });
        }
        total = total
            .checked_add(setting.count)
            .ok_or(ConfigRefusal::WarmSetExceedsPool {
                declared: u32::MAX,
                pool_size,
            })?;
        resolved.push(WarmClass {
            class,
            count: setting.count,
        });
    }
    if total > pool_size {
        return Err(ConfigRefusal::WarmSetExceedsPool {
            declared: total,
            pool_size,
        });
    }
    Ok(resolved)
}

fn validate_system_prompt(prompt: &str) -> Result<(), ConfigRefusal> {
    if prompt.is_empty() {
        return Err(ConfigRefusal::EmptySystemPrompt);
    }
    if prompt.len() > MAX_SYSTEM_PROMPT_BYTES {
        return Err(ConfigRefusal::SystemPromptTooLarge {
            bytes: prompt.len(),
            maximum: MAX_SYSTEM_PROMPT_BYTES,
        });
    }
    // NUL truncates the file the prompt is materialized into; ESC and the rest
    // of C0 are terminal-control payloads in a file pmux writes and a child
    // reads. Newline and tab are the two an operator legitimately needs.
    if let Some(offending) = prompt.chars().find(|character| {
        *character == '\0' || (character.is_control() && *character != '\n' && *character != '\t')
    }) {
        return Err(ConfigRefusal::SystemPromptControlCharacter {
            codepoint: offending as u32,
        });
    }
    Ok(())
}

fn require_absolute(path: &Path, field: ConfigField) -> Result<(), ConfigRefusal> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(ConfigRefusal::RelativePath {
            field,
            path: path.to_path_buf(),
        })
    }
}

/// A non-cryptographic fingerprint of the daemon system prompt.
///
/// Deliberately named a fingerprint, not a digest: its job is to detect that a
/// live instance was minted under a different prompt than the one configuration
/// now holds, which is drift detection, not authentication. FNV-1a is written
/// out rather than pulled in so this file adds no dependency for a check whose
/// adversary is a config reload.
#[must_use]
pub fn fingerprint(prompt: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in prompt.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Which operator-supplied path was inadmissible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigField {
    ParentDir,
    ClaudeExecutable,
    RetainDir,
    EvidenceDir,
}

impl std::fmt::Display for ConfigField {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::ParentDir => "--pool-parent",
            Self::ClaudeExecutable => "--pool-claude",
            Self::RetainDir => "--pool-retain-dir",
            Self::EvidenceDir => "--pool-evidence-dir",
        };
        formatter.write_str(name)
    }
}

/// Every way the daemon refuses to boot a pool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigRefusal {
    PoolSizeOutOfRange {
        requested: u32,
        maximum: u32,
    },
    RecycleTurnsOutOfRange {
        requested: u32,
        maximum: u32,
    },
    EmptySystemPrompt,
    SystemPromptTooLarge {
        bytes: usize,
        maximum: usize,
    },
    SystemPromptControlCharacter {
        codepoint: u32,
    },
    RelativePath {
        field: ConfigField,
        path: PathBuf,
    },
    DirInsidePoolParent {
        field: ConfigField,
        directory: PathBuf,
        parent_dir: PathBuf,
    },
    ZeroIdleTtl,
    ZeroTurnTimeout,
    ZeroWarmCount {
        model: String,
    },
    WarmClassNotAdmitted {
        model: String,
        refusal: ModelEffortRefusal,
    },
    DuplicateWarmClass {
        class: String,
    },
    WarmSetExceedsPool {
        declared: u32,
        pool_size: u32,
    },
    RssBudgetTooSmall {
        pool_size: u32,
        required_mb: u64,
        budget_mb: u64,
    },
}

impl std::fmt::Display for ConfigRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PoolSizeOutOfRange { requested, maximum } => write!(
                formatter,
                "--pool-size {requested} is outside 1..={maximum}"
            ),
            Self::RecycleTurnsOutOfRange { requested, maximum } => write!(
                formatter,
                "--pool-recycle-turns {requested} is outside 1..={maximum}"
            ),
            Self::EmptySystemPrompt => {
                formatter.write_str("the stateless system prompt must not be empty")
            }
            Self::SystemPromptTooLarge { bytes, maximum } => write!(
                formatter,
                "the stateless system prompt is {bytes} bytes, over the {maximum}-byte bound"
            ),
            Self::SystemPromptControlCharacter { codepoint } => write!(
                formatter,
                "the stateless system prompt carries control character U+{codepoint:04X}"
            ),
            Self::RelativePath { field, path } => {
                write!(
                    formatter,
                    "{field} must be absolute; got {}",
                    path.display()
                )
            }
            Self::DirInsidePoolParent {
                field,
                directory,
                parent_dir,
            } => write!(
                formatter,
                "{field} {} is inside the pool parent {}; evidence must outlive the tree it is taken from",
                directory.display(),
                parent_dir.display()
            ),
            Self::ZeroIdleTtl => {
                formatter.write_str("--pool-idle-ttl-ms must be greater than zero")
            }
            Self::ZeroTurnTimeout => {
                formatter.write_str("--pool-turn-timeout-ms must be greater than zero")
            }
            Self::ZeroWarmCount { model } => write!(
                formatter,
                "the warm set declares model {model} with a count of zero; give it a count of at least one, or drop the --pool-warm for it"
            ),
            Self::WarmClassNotAdmitted { model, refusal } => write!(
                formatter,
                "the warm set declares model {model}, which the pool cannot serve: {refusal}"
            ),
            Self::DuplicateWarmClass { class } => write!(
                formatter,
                "the warm set declares class {class} twice; state each MODEL[/EFFORT] in exactly one --pool-warm and add the counts together"
            ),
            Self::WarmSetExceedsPool {
                declared,
                pool_size,
            } => write!(
                formatter,
                "the warm set declares {declared} instances against a pool of {pool_size}; raise --pool-size to at least {declared} (the cap is {MAX_POOL_SIZE}) or lower the --pool-warm counts to {pool_size} in total"
            ),
            Self::RssBudgetTooSmall {
                pool_size,
                required_mb,
                budget_mb,
            } => write!(
                formatter,
                "a pool of {pool_size} needs {required_mb} MB at the {RSS_CEILING_MB_PER_INSTANCE} MB per-instance ceiling, over the {budget_mb} MB budget; raise --pool-rss-budget-mb to at least {required_mb} on a host that has it, or lower --pool-size to {}",
                budget_mb / RSS_CEILING_MB_PER_INSTANCE
            ),
        }
    }
}

impl std::error::Error for ConfigRefusal {}

/// A validated pool configuration.
///
/// Only constructible through [`PoolSettings::validate`], so possession of one
/// is the proof that every bound was checked -- the same shape as membership in
/// the idle set being the emptiness proof.
#[derive(Clone, Debug)]
pub struct PoolConfig {
    pub pool_size: u32,
    pub recycle_turns: u32,
    pub system_prompt: String,
    pub system_prompt_fingerprint: u64,
    pub instance_idle_ttl_ms: u64,
    pub turn_timeout_ms: u64,
    pub parent_dir: PathBuf,
    pub claude_executable: PathBuf,
    pub retain_dir: Option<PathBuf>,
    /// See [`PoolSettings::evidence_dir`].
    pub evidence_dir: Option<PathBuf>,
    pub warm_set: Vec<WarmClass>,
}

impl PoolConfig {
    /// The declared floor for one class, or zero when the operator declared
    /// none. The TTL sweep never evicts below this; cold swap may take from it
    /// only when nothing else is idle.
    #[must_use]
    pub fn warm_floor(&self, class: InstanceClass) -> u32 {
        self.warm_set
            .iter()
            .find(|warm| warm.class == class)
            .map_or(0, |warm| warm.count)
    }

    /// The declared warm floor summed over every declared class: how many
    /// instances the operator said must exist before any caller arrives.
    ///
    /// Zero is the default and means the operator declared nothing, so an
    /// empty pool is a capacity fact. Non-zero is a DECLARATION, and it is the
    /// one input that tells "this pool holds nothing because nobody asked it
    /// to" apart from "this pool holds nothing and somebody did". Every
    /// consumer of that distinction reads it from here rather than counting
    /// `warm_set` itself, so the health tree's question and the boot-time
    /// promise are folded from the same field.
    ///
    /// Saturating, though `validate` already refuses a declared total above
    /// `pool_size` and `pool_size` above the owner cap: an arithmetic wrap in
    /// a health predicate would be a silent zero, which is the answer this
    /// method exists to stop being given by accident.
    #[must_use]
    pub fn declared_warm_total(&self) -> u32 {
        self.warm_set
            .iter()
            .fold(0_u32, |total, warm| total.saturating_add(warm.count))
    }

    /// A caller deadline may only SHORTEN pmux's wait. Nothing a caller writes
    /// lengthens a correctness deadline.
    #[must_use]
    pub fn effective_deadline_ms(&self, now_ms: u64, requested: Option<u64>) -> u64 {
        let ceiling = now_ms.saturating_add(self.turn_timeout_ms);
        match requested {
            Some(deadline) => deadline.min(ceiling),
            None => ceiling,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> PoolSettings {
        PoolSettings::defaults(
            PathBuf::from("/tmp/pmux-pool"),
            PathBuf::from("/usr/bin/claude"),
        )
    }

    #[test]
    fn the_owner_defaults_validate() {
        let config = settings().validate().expect("defaults must boot");
        assert_eq!(config.pool_size, 15);
        assert_eq!(config.recycle_turns, 50);
        assert_eq!(
            config.system_prompt,
            "The user message is the entire instruction."
        );
        assert!(config.warm_set.is_empty());
    }

    #[test]
    fn the_pool_size_bound_is_refused_at_boot_on_both_ends() {
        for requested in [0, MAX_POOL_SIZE + 1, 100] {
            let mut raw = settings();
            raw.pool_size = requested;
            raw.rss_budget_mb = u64::from(requested).max(1) * RSS_CEILING_MB_PER_INSTANCE;
            assert_eq!(
                raw.validate().expect_err("out of range must refuse"),
                ConfigRefusal::PoolSizeOutOfRange {
                    requested,
                    maximum: MAX_POOL_SIZE,
                }
            );
        }
        for requested in 1..=MAX_POOL_SIZE {
            let mut raw = settings();
            raw.pool_size = requested;
            raw.rss_budget_mb = u64::from(requested) * RSS_CEILING_MB_PER_INSTANCE;
            assert!(raw.validate().is_ok(), "{requested} is inside the bound");
        }
    }

    #[test]
    fn the_recycle_ceiling_is_refused_at_boot() {
        for requested in [0, MAX_RECYCLE_TURNS + 1] {
            let mut raw = settings();
            raw.recycle_turns = requested;
            assert_eq!(
                raw.validate().expect_err("out of range must refuse"),
                ConfigRefusal::RecycleTurnsOutOfRange {
                    requested,
                    maximum: MAX_RECYCLE_TURNS,
                }
            );
        }
        let mut raw = settings();
        raw.recycle_turns = MAX_RECYCLE_TURNS;
        assert!(raw.validate().is_ok());
    }

    #[test]
    fn the_system_prompt_bound_is_bytes_and_is_refused_at_boot() {
        let mut raw = settings();
        raw.system_prompt = String::new();
        assert_eq!(
            raw.validate().expect_err("empty must refuse"),
            ConfigRefusal::EmptySystemPrompt
        );

        let mut raw = settings();
        raw.system_prompt = "a".repeat(MAX_SYSTEM_PROMPT_BYTES);
        assert!(raw.validate().is_ok(), "exactly the bound is admitted");

        let mut raw = settings();
        raw.system_prompt = "a".repeat(MAX_SYSTEM_PROMPT_BYTES + 1);
        assert_eq!(
            raw.validate().expect_err("one over must refuse"),
            ConfigRefusal::SystemPromptTooLarge {
                bytes: MAX_SYSTEM_PROMPT_BYTES + 1,
                maximum: MAX_SYSTEM_PROMPT_BYTES,
            }
        );

        // The bound is BYTES, not characters: a prompt of 200 multi-byte
        // characters is over 512 bytes and must refuse, which is exactly the
        // case a character counter would admit.
        let mut raw = settings();
        raw.system_prompt = "\u{1f600}".repeat(200);
        assert!(matches!(
            raw.validate(),
            Err(ConfigRefusal::SystemPromptTooLarge { .. })
        ));
    }

    #[test]
    fn a_correct_prompt_containing_an_abbreviation_is_admitted() {
        // The case a sentence counter gets wrong, asserted as a fact so nobody
        // re-adds one.
        let mut raw = settings();
        raw.system_prompt =
            "Answer directly, e.g. with the value itself. If you cannot answer, say so in one line."
                .to_owned();
        assert!(raw.validate().is_ok());
    }

    #[test]
    fn control_characters_in_the_system_prompt_are_refused() {
        for (character, admitted) in [
            ('\0', false),
            ('\u{1b}', false),
            ('\u{7}', false),
            ('\n', true),
            ('\t', true),
        ] {
            let mut raw = settings();
            raw.system_prompt = format!("Answer directly{character}and completely.");
            let outcome = raw.validate();
            assert_eq!(
                outcome.is_ok(),
                admitted,
                "U+{:04X} admitted={admitted}",
                character as u32
            );
        }
    }

    #[test]
    fn relative_operator_paths_are_refused_at_boot() {
        let mut raw = settings();
        raw.parent_dir = PathBuf::from("pool");
        assert!(matches!(
            raw.validate(),
            Err(ConfigRefusal::RelativePath {
                field: ConfigField::ParentDir,
                ..
            })
        ));

        let mut raw = settings();
        raw.claude_executable = PathBuf::from("claude");
        assert!(matches!(
            raw.validate(),
            Err(ConfigRefusal::RelativePath {
                field: ConfigField::ClaudeExecutable,
                ..
            })
        ));

        let mut raw = settings();
        raw.retain_dir = Some(PathBuf::from("evidence"));
        assert!(matches!(
            raw.validate(),
            Err(ConfigRefusal::RelativePath {
                field: ConfigField::RetainDir,
                ..
            })
        ));
    }

    #[test]
    fn a_retain_dir_inside_the_pool_parent_is_refused() {
        let mut raw = settings();
        raw.retain_dir = Some(PathBuf::from("/tmp/pmux-pool/evidence"));
        assert!(matches!(
            raw.validate(),
            Err(ConfigRefusal::DirInsidePoolParent {
                field: ConfigField::RetainDir,
                ..
            })
        ));
    }

    #[test]
    fn the_rss_boot_assertion_refuses_an_undersized_host() {
        let mut raw = settings();
        raw.pool_size = 15;
        raw.rss_budget_mb = 15 * RSS_CEILING_MB_PER_INSTANCE - 1;
        assert_eq!(
            raw.validate().expect_err("an undersized host must refuse"),
            ConfigRefusal::RssBudgetTooSmall {
                pool_size: 15,
                required_mb: 15 * RSS_CEILING_MB_PER_INSTANCE,
                budget_mb: 15 * RSS_CEILING_MB_PER_INSTANCE - 1,
            }
        );

        let mut raw = settings();
        raw.pool_size = 15;
        raw.rss_budget_mb = 15 * RSS_CEILING_MB_PER_INSTANCE;
        assert!(raw.validate().is_ok(), "exactly the budget is admitted");
    }

    #[test]
    fn the_warm_set_is_resolved_through_the_pool_rule_at_boot() {
        let mut raw = settings();
        raw.warm_set = vec![
            WarmClassSetting {
                model: "opus".to_owned(),
                effort: Some(EffortLevel::High),
                count: 2,
            },
            WarmClassSetting {
                model: "claude-haiku-4-5".to_owned(),
                effort: None,
                count: 3,
            },
        ];
        let config = raw.validate().expect("an admissible warm set boots");
        assert_eq!(config.warm_set.len(), 2);
        assert_eq!(
            config.warm_floor(config.warm_set[0].class),
            2,
            "the declared floor is readable by class"
        );
        assert_eq!(
            config.declared_warm_total(),
            5,
            "the declared floor is also readable as a total, which is what tells an empty pool \
             nobody asked to hold anything from an empty pool that was told to hold five"
        );
    }

    /// An undeclared warm set totals ZERO, and that zero is load-bearing: it is
    /// what makes a cold pool vacuous rather than faulted in the health tree.
    /// The default is the case that must never drift, because it is every
    /// daemon that gave `--pool-parent` and no `--pool-warm`.
    #[test]
    fn an_undeclared_warm_set_totals_zero() {
        let config = settings().validate().expect("defaults must boot");
        assert!(config.warm_set.is_empty());
        assert_eq!(config.declared_warm_total(), 0);
    }

    #[test]
    fn an_inadmissible_warm_class_refuses_at_boot_rather_than_at_mint() {
        let mut raw = settings();
        raw.warm_set = vec![WarmClassSetting {
            model: "claude-haiku-4-5".to_owned(),
            effort: Some(EffortLevel::High),
            count: 1,
        }];
        assert!(matches!(
            raw.validate(),
            Err(ConfigRefusal::WarmClassNotAdmitted { .. })
        ));

        let mut raw = settings();
        raw.warm_set = vec![WarmClassSetting {
            model: "claude-invented-9".to_owned(),
            effort: None,
            count: 1,
        }];
        assert!(matches!(
            raw.validate(),
            Err(ConfigRefusal::WarmClassNotAdmitted { .. })
        ));
    }

    #[test]
    fn a_warm_set_larger_than_the_pool_is_refused_at_boot() {
        let mut raw = settings();
        raw.pool_size = 2;
        raw.rss_budget_mb = 2 * RSS_CEILING_MB_PER_INSTANCE;
        raw.warm_set = vec![WarmClassSetting {
            model: "opus".to_owned(),
            effort: None,
            count: 3,
        }];
        assert_eq!(
            raw.validate()
                .expect_err("an oversized warm set must refuse"),
            ConfigRefusal::WarmSetExceedsPool {
                declared: 3,
                pool_size: 2,
            }
        );
    }

    /// A warm set that exactly FILLS the pool is admitted, and one instance
    /// more is refused.
    ///
    /// SURVIVING MUTANT CLOSED: `config.rs:262 > -> >=` in `resolve_warm_set`.
    /// The test above declares 3 against a pool of 2, and `3 > 2` and `3 >= 2`
    /// agree about that -- so the one value that tells the two apart, a warm
    /// set the size of the pool, was the value never tried. Under `>=` an
    /// operator who declares exactly as many warm instances as they configured
    /// slots is refused at boot with "declared 2 against a pool of 2", which is
    /// both the most natural configuration anybody would write and the one this
    /// feature exists for: paying every launch once at boot instead of on a
    /// caller's first request.
    #[test]
    fn a_warm_set_that_exactly_fills_the_pool_is_admitted_and_one_more_is_not() {
        let mut raw = settings();
        raw.pool_size = 2;
        raw.rss_budget_mb = 2 * RSS_CEILING_MB_PER_INSTANCE;
        raw.warm_set = vec![WarmClassSetting {
            model: "opus".to_owned(),
            effort: None,
            count: 2,
        }];
        let filled = raw
            .validate()
            .expect("a warm set the size of the pool is what a declared warm set is for");
        assert_eq!(
            filled.warm_set.iter().map(|warm| warm.count).sum::<u32>(),
            2,
            "the whole declared set survives validation"
        );

        // ...and the bound really is a bound: one past it refuses, naming both
        // numbers, so this test cannot be satisfied by a check that never fires.
        let mut raw = settings();
        raw.pool_size = 2;
        raw.rss_budget_mb = 2 * RSS_CEILING_MB_PER_INSTANCE;
        raw.warm_set = vec![WarmClassSetting {
            model: "opus".to_owned(),
            effort: None,
            count: 3,
        }];
        assert_eq!(
            raw.validate()
                .expect_err("one instance past the pool size must refuse"),
            ConfigRefusal::WarmSetExceedsPool {
                declared: 3,
                pool_size: 2,
            }
        );
    }

    #[test]
    fn a_duplicated_warm_class_is_refused_at_boot() {
        let mut raw = settings();
        raw.warm_set = vec![
            WarmClassSetting {
                model: "opus".to_owned(),
                effort: Some(EffortLevel::High),
                count: 1,
            },
            WarmClassSetting {
                model: "claude-opus-5".to_owned(),
                effort: Some(EffortLevel::High),
                count: 1,
            },
        ];
        assert!(matches!(
            raw.validate(),
            Err(ConfigRefusal::DuplicateWarmClass { .. })
        ));
    }

    #[test]
    fn a_zero_warm_count_is_refused_at_boot() {
        let mut raw = settings();
        raw.warm_set = vec![WarmClassSetting {
            model: "opus".to_owned(),
            effort: None,
            count: 0,
        }];
        assert!(matches!(
            raw.validate(),
            Err(ConfigRefusal::ZeroWarmCount { .. })
        ));
    }

    #[test]
    fn a_caller_deadline_may_only_shorten_the_wait() {
        let config = settings().validate().expect("defaults boot");
        let now = 1_000;
        let ceiling = now + config.turn_timeout_ms;
        assert_eq!(config.effective_deadline_ms(now, None), ceiling);
        assert_eq!(config.effective_deadline_ms(now, Some(now + 10)), now + 10);
        assert_eq!(
            config.effective_deadline_ms(now, Some(ceiling + 1_000_000)),
            ceiling,
            "a caller cannot lengthen a correctness deadline by asking nicely"
        );
    }

    #[test]
    fn the_prompt_fingerprint_separates_two_prompts() {
        assert_ne!(
            fingerprint(DEFAULT_SYSTEM_PROMPT),
            fingerprint("Answer directly and completely."),
        );
        assert_eq!(
            fingerprint(DEFAULT_SYSTEM_PROMPT),
            fingerprint(DEFAULT_SYSTEM_PROMPT)
        );
    }

    /// The fingerprint is FNV-1a, asserted against the published vectors rather
    /// than against itself.
    ///
    /// SURVIVING MUTANT CLOSED: `config.rs:318 ^= -> |=` in [`fingerprint`].
    /// The test above asserts only that two particular prompts differ and that
    /// one prompt is stable, and `|=` satisfies both: it is still a
    /// deterministic function of the bytes and it still separates those two
    /// strings. It is not FNV-1a, and it is far weaker -- `|=` can never clear
    /// a bit, so every byte only ever sets bits and the mixing the algorithm
    /// depends on is gone. What that costs is the thing the fingerprint exists
    /// for: `Instance::check_invariants` refuses an idle instance whose
    /// `prompt_fingerprint` differs from live configuration, so a weak
    /// fingerprint returns instances to service under a system prompt the
    /// daemon no longer holds.
    ///
    /// MEASURED on this host: `fingerprint("a")` is `0xaf63_dc4c_8601_ec8c`
    /// under `^=` and `0xaf63_fd4c_8602_249f` under `|=`.
    #[test]
    fn the_prompt_fingerprint_is_fnv_1a_and_not_merely_some_function_of_the_bytes() {
        // The published 64-bit FNV-1a vectors. Nothing in this file derives
        // them, which is the whole point of a known-answer test: an expectation
        // computed from the implementation cannot detect a wrong one.
        assert_eq!(fingerprint(""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fingerprint("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fingerprint("foobar"), 0x8594_4171_f739_67e8);
    }
}
