//! The pool's class key, and the one rule that renders `--model` / `--effort`.
//!
//! `--model` and `--effort` are launch-time argv: `/clear` rotates a transcript,
//! it does not re-exec. So "any instance serves any turn" is FALSE once model
//! and effort are caller inputs, and fungibility is per class:
//!
//! ```text
//! InstanceClass = (canonical_model_argv, effort_argv)
//! ```
//!
//! Instances are fungible *within* a class and never across one. The pool is
//! therefore keyed by class, not a single queue -- a single queue hands an
//! opus/max call to a haiku/low process.
//!
//! The class key must be computed by the SAME call that builds argv, or the two
//! drift and the pool's model of an instance stops being a fact about the
//! process. [`resolve_model_effort`] is that call: it is the only expression in
//! this module that produces an `--effort` value, and it can only produce one by
//! finding it in a [`ModelEntry`]'s admitted set.

use std::fmt;

use pseudomux_protocol::v1::{EffortLevel, ErrorBody, ErrorCode};
use serde_json::json;

/// One admitted effort tier, paired with the exact argv token it renders to.
///
/// The pairing lives on the table rather than in a `match EffortLevel` because
/// the argv token must be reachable ONLY through an admitted-set membership
/// test. There is deliberately no expression in this module that turns an
/// `EffortLevel` into an `--effort` value on its own, so "admitted" and
/// "renderable" are the same fact rather than two facts that can drift.
///
/// It is also why a tier no model admits has no spelling here at all: a variant
/// that is not in any set is refused by the same code path that refuses a tier
/// a particular model does not take, with no arm of its own to forget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmittedEffort {
    pub level: EffortLevel,
    pub argv: &'static str,
}

const LOW: AdmittedEffort = AdmittedEffort {
    level: EffortLevel::Low,
    argv: "low",
};
const MEDIUM: AdmittedEffort = AdmittedEffort {
    level: EffortLevel::Medium,
    argv: "medium",
};
const HIGH: AdmittedEffort = AdmittedEffort {
    level: EffortLevel::High,
    argv: "high",
};
const XHIGH: AdmittedEffort = AdmittedEffort {
    level: EffortLevel::XHigh,
    argv: "xhigh",
};
const MAX: AdmittedEffort = AdmittedEffort {
    level: EffortLevel::Max,
    argv: "max",
};

/// This model REJECTS `--effort` entirely.
///
/// Reject-by-default is the same polarity `assert_empty_at_launch` defends: a
/// model with no admitted tier admits no effort at all, rather than silently
/// dropping the flag and launching a child the caller did not ask for.
const EFFORTS_NONE: &[AdmittedEffort] = &[];
/// The 4.5 Opus generation: `low`/`medium`/`high`, no `xhigh` and no `max`.
const EFFORTS_THROUGH_HIGH: &[AdmittedEffort] = &[LOW, MEDIUM, HIGH];
/// The 4.6 generation: `max` but no `xhigh` -- `xhigh` arrived with Opus 4.7.
const EFFORTS_THROUGH_MAX: &[AdmittedEffort] = &[LOW, MEDIUM, HIGH, MAX];
/// The 4.7-and-later generations: the full ladder.
const EFFORTS_ALL: &[AdmittedEffort] = &[LOW, MEDIUM, HIGH, XHIGH, MAX];

/// One admitted model, its spellings, and the effort tiers it takes.
#[derive(Debug, PartialEq, Eq)]
pub struct ModelEntry {
    /// The exact `--model` argv value. This, not the caller's string, is half
    /// the class key -- so two spellings of one model resolve to one class and
    /// cannot burn two slots.
    pub canonical: &'static str,
    /// Alternate spellings admitted for `canonical`. Matched ASCII
    /// case-insensitively, for the same reason: a caller that shouts the model
    /// name must not partition the pool.
    pub aliases: &'static [&'static str],
    /// EMPTY means: this model REJECTS `--effort`.
    pub efforts: &'static [AdmittedEffort],
}

impl ModelEntry {
    fn matches(&self, spelling: &str) -> bool {
        self.canonical.eq_ignore_ascii_case(spelling)
            || self
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(spelling))
    }

    fn admit(&self, effort: EffortLevel) -> Option<&'static AdmittedEffort> {
        self.efforts.iter().find(|entry| entry.level == effort)
    }

    /// The admitted set, in the spellings `--effort` accepts.
    ///
    /// These are [`AdmittedEffort::argv`] values and nothing else, so the words
    /// a refusal offers an operator are exactly the words that model's argv can
    /// be rendered from. A model that takes no tier yields an empty vector and
    /// never reaches `EffortNotAdmitted` at all -- `ModelTakesNoEffort` is the
    /// arm for that, so there is no "it admits nothing" sentence to get wrong.
    #[must_use]
    pub fn admitted_spellings(&self) -> Vec<&'static str> {
        self.efforts.iter().map(|entry| entry.argv).collect()
    }
}

/// Every Claude model pmux admits, at its real effort tiers.
///
/// CHOSEN, not MEASURED: the tier memberships come from the published model
/// catalogue, not from a probe against the installed bundle. They must be
/// probed before this table is pinned to a Claude version -- one
/// `--model <M> --effort <E>` probe per cell, recorded with the version.
/// Getting a row wrong makes an admitted request fail at launch, which is the
/// diagnostic this table exists to eliminate.
///
/// The table is compile-time rather than protocol so a new Anthropic model is
/// an operator change, not a three-language protocol event.
pub static MODEL_TABLE: &[ModelEntry] = &[
    ModelEntry {
        canonical: "claude-opus-5",
        aliases: &["opus", "opus-5"],
        efforts: EFFORTS_ALL,
    },
    ModelEntry {
        canonical: "claude-opus-4-8",
        aliases: &["opus-4-8", "opus-4.8"],
        efforts: EFFORTS_ALL,
    },
    ModelEntry {
        canonical: "claude-opus-4-7",
        aliases: &["opus-4-7", "opus-4.7"],
        efforts: EFFORTS_ALL,
    },
    ModelEntry {
        canonical: "claude-opus-4-6",
        aliases: &["opus-4-6", "opus-4.6"],
        efforts: EFFORTS_THROUGH_MAX,
    },
    ModelEntry {
        canonical: "claude-opus-4-5",
        aliases: &["opus-4-5", "opus-4.5"],
        efforts: EFFORTS_THROUGH_HIGH,
    },
    ModelEntry {
        canonical: "claude-sonnet-5",
        aliases: &["sonnet", "sonnet-5"],
        efforts: EFFORTS_ALL,
    },
    ModelEntry {
        canonical: "claude-sonnet-4-6",
        aliases: &["sonnet-4-6", "sonnet-4.6"],
        efforts: EFFORTS_THROUGH_MAX,
    },
    ModelEntry {
        canonical: "claude-sonnet-4-5",
        aliases: &["sonnet-4-5", "sonnet-4.5"],
        efforts: EFFORTS_NONE,
    },
    ModelEntry {
        canonical: "claude-haiku-4-5",
        aliases: &["haiku", "haiku-4-5", "haiku-4.5"],
        efforts: EFFORTS_NONE,
    },
];

fn lookup(spelling: &str) -> Option<&'static ModelEntry> {
    MODEL_TABLE.iter().find(|entry| entry.matches(spelling))
}

/// Every canonical `--model` value this pool admits, for a refusal to offer.
///
/// Derived from [`MODEL_TABLE`], so a model added to the table is offered by
/// every refusal without anyone editing a sentence.
fn admitted_model_list() -> String {
    MODEL_TABLE
        .iter()
        .map(|entry| entry.canonical)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The pool's fungibility key.
///
/// Both halves are `&'static str` from [`MODEL_TABLE`], never a caller string,
/// so a class is exactly the argv pair a process was launched with. Ordered so
/// the pool's maps iterate deterministically and a test can name a class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstanceClass {
    pub canonical_model: &'static str,
    pub effort_argv: Option<&'static str>,
}

impl InstanceClass {
    /// The class key, built from the resolved table entry rather than from the
    /// caller's spelling. This is the constructor the pool uses and the only
    /// one; there is deliberately no `InstanceClass::new(&str, &str)`, because
    /// a class assembled from strings is a class that can disagree with argv.
    #[must_use]
    pub fn of(resolved: &ResolvedModelEffort) -> Option<Self> {
        Some(Self {
            canonical_model: resolved.entry?.canonical,
            effort_argv: resolved.effort_arg,
        })
    }

    /// The typed effort tier this class renders, recovered from its argv token.
    ///
    /// Recovered rather than stored beside `effort_argv`. A stored copy is a
    /// second fact about one thing, and the thing it is a second fact about is
    /// the class KEY -- the basis of every fungibility decision the pool makes.
    /// The two would be free to disagree, and the disagreement would be
    /// invisible: `Eq`, `Hash` and `Ord` are all derived from the pair.
    ///
    /// The search universe is every `AdmittedEffort` reachable from
    /// [`MODEL_TABLE`], which is EXACTLY the set `effort_argv` was drawn from.
    /// It is not a `match EffortLevel`, and not `EFFORTS_ALL` (private, so not
    /// an intra-doc link from public documentation): the first is a
    /// second spelling table, and the second is one model generation's ladder,
    /// so a tier admitted by some future entry and absent from it would come
    /// back `None` from a class that renders it.
    #[must_use]
    pub fn effort_level(&self) -> Option<EffortLevel> {
        let argv = self.effort_argv?;
        MODEL_TABLE
            .iter()
            .flat_map(|entry| entry.efforts)
            .find(|admitted| admitted.argv == argv)
            .map(|admitted| admitted.level)
    }

    /// The argv fragment this class renders to, in order. The pool records it
    /// so a test can assert an instance's class renders byte-identically to the
    /// argv its process was spawned with.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        let mut args = vec!["--model".to_owned(), self.canonical_model.to_owned()];
        if let Some(effort) = self.effort_argv {
            args.push("--effort".to_owned());
            args.push(effort.to_owned());
        }
        args
    }
}

impl fmt::Display for InstanceClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.effort_argv {
            Some(effort) => write!(formatter, "{}/{effort}", self.canonical_model),
            None => write!(formatter, "{}/-", self.canonical_model),
        }
    }
}

/// What one `(model, effort)` pair renders to, and the entry it resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedModelEffort {
    /// Exactly what goes after `--model`, or `None` when the caller named none.
    ///
    /// A `String` rather than `&'static str` because Path A must keep working
    /// with a model pmux has never heard of: an unknown model with no effort
    /// passes through verbatim. Path B refuses that case separately, because a
    /// pass-through model has no table entry and therefore no class key.
    pub model_arg: Option<String>,
    /// Exactly what goes after `--effort`, or `None` when nothing is rendered.
    pub effort_arg: Option<&'static str>,
    /// The canonical table entry, when the model resolved to one.
    pub entry: Option<&'static ModelEntry>,
    /// The admitted level that produced `effort_arg`. Published back on the
    /// wire so a caller learns what pmux actually rendered.
    pub effort_level: Option<EffortLevel>,
}

/// Why a `(model, effort)` pair was refused.
///
/// A typed error rather than an `ErrorBody` so the argv render site can carry it
/// through `anyhow` while the pool converts it to the wire refusal, and neither
/// has to restate the rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelEffortRefusal {
    /// `--effort` with no `--model`. Validating a tier against a model pmux was
    /// never told is unsatisfiable, and resolving the operator's default by
    /// running Claude would put a subprocess on the admission path.
    EffortWithoutModel { effort: EffortLevel },
    /// An explicit effort against a model that is not in the table. You cannot
    /// honestly validate a tier against a model you do not know, and the
    /// alternative surfaces as a launch failure or a turn-1 hang.
    UnknownModelWithEffort { model: String, effort: EffortLevel },
    /// A model whose admitted set is empty.
    ModelTakesNoEffort {
        model: &'static str,
        effort: EffortLevel,
    },
    /// A model that takes effort, but not this one.
    EffortNotAdmitted {
        model: &'static str,
        effort: EffortLevel,
        admitted: Vec<&'static str>,
    },
    /// Path B only: a model that is not a table entry has no class key, so two
    /// spellings of one model would become two classes and burn two slots.
    /// This is a statement about the pool key, not a second copy of the argv
    /// rule -- Path A still admits an unknown model with no effort.
    UnknownModelForPool { model: String },
}

impl ModelEffortRefusal {
    /// The wire refusal. Every arm is [`ErrorCode::InvalidConfig`], because
    /// every arm is a request pmux cannot honestly execute; none is retryable.
    #[must_use]
    pub fn into_error_body(self) -> ErrorBody {
        let message = self.to_string();
        // `admitted_models` has always been on the wire and DERIVED from
        // `MODEL_TABLE`, but nothing rendered it to a person: `pmux` prints
        // `recommendation` and no other `details` key, because `details` also
        // carries capability tokens. So the same derived list is written into
        // the advice channel, which is the one thing a CLI caller sees.
        let admitted = admitted_model_list();
        let details = match &self {
            Self::EffortWithoutModel { .. }
            | Self::UnknownModelWithEffort { .. }
            | Self::ModelTakesNoEffort { .. }
            | Self::EffortNotAdmitted { .. } => json!({
                "violation": "model_effort_not_admitted",
                "admitted_models": MODEL_TABLE
                    .iter()
                    .map(|entry| entry.canonical)
                    .collect::<Vec<_>>(),
                "recommendation": format!(
                    "the models this pool admits are {admitted}; --effort is validated against \
                     the resolved model, so a tier one model takes is not a tier they all take"
                ),
            }),
            Self::UnknownModelForPool { .. } => json!({
                "violation": "model_not_admitted_to_pool",
                "admitted_models": MODEL_TABLE
                    .iter()
                    .map(|entry| entry.canonical)
                    .collect::<Vec<_>>(),
                "recommendation": format!("name one of {admitted}, or one of their aliases"),
            }),
        };
        ErrorBody::new(ErrorCode::InvalidConfig, message).with_details(details)
    }
}

/// Every refusal renders the tier with [`EffortLevel::as_str`] and never with
/// `{effort:?}`.
///
/// MEASURED before the change: `EffortNotAdmitted` produced *"model
/// claude-haiku-4-5 does not admit --effort XHigh; it admits [\"low\",
/// \"medium\", \"high\", \"max\"]"*. One sentence spelled the concept two ways,
/// and the one immediately after the literal `--effort` -- the only one an
/// operator can be expected to copy -- is rejected by clap and by
/// `EffortLevel`'s own `Deserialize`. A refusal that tells you to use a flag
/// value nothing accepts costs the reader the retry it was written to save.
///
/// This does not weaken the module rule stated at the top of the file. That
/// rule is about the argv token pmux HANDS TO CLAUDE, which is still reachable
/// only through [`AdmittedEffort::argv`] and therefore only through an
/// admitted-set membership test. Every tier named here has just been REFUSED,
/// so by construction none of them is renderable as argv; what is being spelled
/// is the caller's own word, echoed back in the spelling the caller's own
/// interface uses.
impl fmt::Display for ModelEffortRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EffortWithoutModel { effort } => write!(
                formatter,
                "--effort {effort} requires an explicit model: effort tiers are not uniform across Claude models"
            ),
            Self::UnknownModelWithEffort { model, effort } => write!(
                formatter,
                "model {model} is not admitted, so --effort {effort} cannot be validated against it"
            ),
            Self::ModelTakesNoEffort { model, effort } => write!(
                formatter,
                "model {model} takes no effort tier; --effort {effort} is refused rather than silently dropped"
            ),
            Self::EffortNotAdmitted {
                model,
                effort,
                admitted,
            } => write!(
                formatter,
                "model {model} does not admit --effort {effort}; it admits {}",
                admitted.join(", ")
            ),
            Self::UnknownModelForPool { model } => write!(
                formatter,
                "model {model} is not admitted to the stateless pool: a model with no table entry has no instance class"
            ),
        }
    }
}

impl std::error::Error for ModelEffortRefusal {}

/// The one rule, called once.
///
/// The pool's instinct is an early validity check so a bad request cannot evict
/// an idle instance. An early check that RESTATES the rule is the drift hazard;
/// an early check that IS the rule is not. So the pool computes its class key by
/// calling this, and the "early check" and the "real check" are literally the
/// same function with the same inputs.
///
/// # Errors
///
/// Returns [`ModelEffortRefusal`] for every pair pmux cannot honestly render.
pub fn resolve_model_effort(
    model: Option<&str>,
    effort: Option<EffortLevel>,
) -> Result<ResolvedModelEffort, ModelEffortRefusal> {
    match (model, effort) {
        // The arm everybody forgets. `--effort high` with no `--model` would
        // validate a tier against whatever the operator's `.claude.json`
        // default resolves to, which is a model pmux never named.
        (None, Some(effort)) => Err(ModelEffortRefusal::EffortWithoutModel { effort }),
        (None, None) => Ok(ResolvedModelEffort {
            model_arg: None,
            effort_arg: None,
            entry: None,
            effort_level: None,
        }),
        (Some(model), effort) => resolve_named(model, effort),
    }
}

fn resolve_named(
    model: &str,
    effort: Option<EffortLevel>,
) -> Result<ResolvedModelEffort, ModelEffortRefusal> {
    match (lookup(model), effort) {
        // Path A keeps working with a model pmux has never heard of, as long as
        // it names no tier there is nothing to validate against.
        (None, None) => Ok(ResolvedModelEffort {
            model_arg: Some(model.to_owned()),
            effort_arg: None,
            entry: None,
            effort_level: None,
        }),
        (None, Some(effort)) => Err(ModelEffortRefusal::UnknownModelWithEffort {
            model: model.to_owned(),
            effort,
        }),
        (Some(entry), None) => Ok(ResolvedModelEffort {
            model_arg: Some(entry.canonical.to_owned()),
            effort_arg: None,
            entry: Some(entry),
            effort_level: None,
        }),
        (Some(entry), Some(effort)) => match entry.admit(effort) {
            Some(admitted) => Ok(ResolvedModelEffort {
                model_arg: Some(entry.canonical.to_owned()),
                effort_arg: Some(admitted.argv),
                entry: Some(entry),
                effort_level: Some(admitted.level),
            }),
            None if entry.efforts.is_empty() => Err(ModelEffortRefusal::ModelTakesNoEffort {
                model: entry.canonical,
                effort,
            }),
            None => Err(ModelEffortRefusal::EffortNotAdmitted {
                model: entry.canonical,
                effort,
                admitted: entry.admitted_spellings(),
            }),
        },
    }
}

/// The pool's additional requirement, stated once.
///
/// Path B needs the model to be a table entry even when effort is absent,
/// because the class key comes from the entry. One line, and it is a statement
/// about the pool key rather than a second copy of the argv rule.
///
/// # Errors
///
/// Returns [`ModelEffortRefusal::UnknownModelForPool`] when the resolved model
/// has no table entry.
pub fn resolve_pool_class(
    model: &str,
    effort: Option<EffortLevel>,
) -> Result<(InstanceClass, ResolvedModelEffort), ModelEffortRefusal> {
    let resolved = resolve_model_effort(Some(model), effort)?;
    let class =
        InstanceClass::of(&resolved).ok_or_else(|| ModelEffortRefusal::UnknownModelForPool {
            model: model.to_owned(),
        })?;
    Ok((class, resolved))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `EffortLevel` a caller can name, so the table below is a complete
    /// matrix rather than a sample. Written as a list rather than derived,
    /// because a variant added to the protocol without a decision here should
    /// surface as a missing row in `every_effort_variant_is_covered`.
    const NAMED_EFFORTS: &[EffortLevel] = &[
        EffortLevel::Low,
        EffortLevel::Medium,
        EffortLevel::High,
        EffortLevel::XHigh,
        EffortLevel::Max,
    ];

    #[test]
    fn an_effort_argv_token_matches_the_wire_spelling_of_its_level() {
        // The argv token and the wire spelling are the same string for a
        // reason: `--effort xhigh` is what the child takes, and `"xhigh"` is
        // what the request carries. Deriving one from the other by hand is how
        // they drift, so this asserts they agree without either being written
        // in terms of the other.
        for admitted in EFFORTS_ALL {
            let wire = serde_json::to_value(admitted.level).expect("effort level is serializable");
            assert_eq!(
                wire,
                serde_json::Value::String(admitted.argv.to_owned()),
                "argv token for {:?} must equal its wire spelling",
                admitted.level
            );
        }
    }

    #[test]
    fn every_effort_variant_is_covered_by_the_full_set() {
        for effort in NAMED_EFFORTS {
            assert!(
                EFFORTS_ALL.iter().any(|admitted| admitted.level == *effort),
                "{effort:?} has no argv spelling in the full admitted set"
            );
        }
    }

    #[test]
    fn effort_without_a_model_is_refused_for_both_paths() {
        for effort in NAMED_EFFORTS {
            let refusal = resolve_model_effort(None, Some(*effort))
                .expect_err("effort without a model must refuse");
            assert_eq!(
                refusal,
                ModelEffortRefusal::EffortWithoutModel { effort: *effort }
            );
        }
    }

    #[test]
    fn neither_a_model_nor_an_effort_renders_nothing() {
        let resolved = resolve_model_effort(None, None).expect("the empty pair is admitted");
        assert_eq!(resolved.model_arg, None);
        assert_eq!(resolved.effort_arg, None);
        assert!(resolved.entry.is_none());
    }

    #[test]
    fn an_unknown_model_passes_through_without_an_effort_and_refuses_with_one() {
        let resolved = resolve_model_effort(Some("claude-invented-9"), None)
            .expect("an unknown model with no effort keeps Path A working");
        assert_eq!(resolved.model_arg.as_deref(), Some("claude-invented-9"));
        assert_eq!(resolved.effort_arg, None);
        assert!(resolved.entry.is_none());

        for effort in NAMED_EFFORTS {
            let refusal = resolve_model_effort(Some("claude-invented-9"), Some(*effort))
                .expect_err("an unknown model with an effort must refuse");
            assert_eq!(
                refusal,
                ModelEffortRefusal::UnknownModelWithEffort {
                    model: "claude-invented-9".to_owned(),
                    effort: *effort,
                }
            );
        }
    }

    #[test]
    fn the_complete_model_by_effort_matrix_renders_exactly_or_refuses_exactly() {
        for entry in MODEL_TABLE {
            for spelling in std::iter::once(entry.canonical).chain(entry.aliases.iter().copied()) {
                let resolved = resolve_model_effort(Some(spelling), None)
                    .expect("a table entry with no effort is admitted");
                assert_eq!(
                    resolved.model_arg.as_deref(),
                    Some(entry.canonical),
                    "{spelling} must render its canonical argv"
                );
                assert_eq!(resolved.effort_arg, None);

                for effort in NAMED_EFFORTS {
                    let outcome = resolve_model_effort(Some(spelling), Some(*effort));
                    match entry.admit(*effort) {
                        Some(admitted) => {
                            let resolved = outcome.expect("an admitted tier renders");
                            assert_eq!(resolved.model_arg.as_deref(), Some(entry.canonical));
                            assert_eq!(resolved.effort_arg, Some(admitted.argv));
                            assert_eq!(resolved.effort_level, Some(*effort));
                        }
                        None if entry.efforts.is_empty() => assert_eq!(
                            outcome.expect_err("a model with no tiers refuses every effort"),
                            ModelEffortRefusal::ModelTakesNoEffort {
                                model: entry.canonical,
                                effort: *effort,
                            }
                        ),
                        None => assert_eq!(
                            outcome.expect_err("an unadmitted tier refuses"),
                            ModelEffortRefusal::EffortNotAdmitted {
                                model: entry.canonical,
                                effort: *effort,
                                admitted: entry.admitted_spellings(),
                            }
                        ),
                    }
                }
            }
        }
    }

    #[test]
    fn the_named_generations_admit_exactly_the_tiers_they_were_pinned_to() {
        // The three shapes stated as facts about named models, so a table edit
        // that widens a generation is a red test rather than a silent policy
        // change.
        assert!(resolve_model_effort(Some("claude-haiku-4-5"), Some(EffortLevel::Low)).is_err());
        assert!(resolve_model_effort(Some("claude-sonnet-4-5"), Some(EffortLevel::Low)).is_err());
        assert!(resolve_model_effort(Some("claude-opus-4-6"), Some(EffortLevel::XHigh)).is_err());
        assert!(resolve_model_effort(Some("claude-sonnet-4-6"), Some(EffortLevel::XHigh)).is_err());
        assert!(resolve_model_effort(Some("claude-opus-4-5"), Some(EffortLevel::Max)).is_err());
        assert!(resolve_model_effort(Some("claude-opus-4-6"), Some(EffortLevel::Max)).is_ok());
        assert!(resolve_model_effort(Some("claude-opus-5"), Some(EffortLevel::XHigh)).is_ok());
        assert!(resolve_model_effort(Some("claude-sonnet-5"), Some(EffortLevel::Max)).is_ok());
    }

    #[test]
    fn two_spellings_of_one_model_resolve_to_one_class() {
        let (canonical, _) =
            resolve_pool_class("claude-opus-5", Some(EffortLevel::High)).expect("canonical");
        let (alias, _) = resolve_pool_class("OPUS", Some(EffortLevel::High)).expect("alias");
        assert_eq!(canonical, alias);
        assert_eq!(canonical.canonical_model, "claude-opus-5");
        assert_eq!(canonical.effort_argv, Some("high"));
    }

    #[test]
    fn a_class_renders_the_argv_its_process_was_spawned_with() {
        let (class, resolved) =
            resolve_pool_class("opus-4.6", Some(EffortLevel::Max)).expect("admitted");
        let mut expected = vec!["--model".to_owned(), resolved.model_arg.clone().unwrap()];
        expected.push("--effort".to_owned());
        expected.push(resolved.effort_arg.unwrap().to_owned());
        assert_eq!(class.argv(), expected);

        let (bare, _) = resolve_pool_class("claude-haiku-4-5", None).expect("admitted");
        assert_eq!(
            bare.argv(),
            vec!["--model".to_owned(), "claude-haiku-4-5".to_owned()]
        );
    }

    #[test]
    fn a_model_the_table_does_not_know_has_no_pool_class() {
        let refusal = resolve_pool_class("claude-invented-9", None)
            .expect_err("the pool refuses a model with no class key");
        assert_eq!(
            refusal,
            ModelEffortRefusal::UnknownModelForPool {
                model: "claude-invented-9".to_owned(),
            }
        );
        // ...while Path A still admits it, which is the whole point of stating
        // the pool rule separately instead of narrowing the argv rule.
        assert!(resolve_model_effort(Some("claude-invented-9"), None).is_ok());
    }

    #[test]
    fn every_refusal_is_an_invalid_config_that_names_the_violation() {
        let bodies = [
            ModelEffortRefusal::EffortWithoutModel {
                effort: EffortLevel::High,
            },
            ModelEffortRefusal::UnknownModelWithEffort {
                model: "x".to_owned(),
                effort: EffortLevel::High,
            },
            ModelEffortRefusal::ModelTakesNoEffort {
                model: "claude-haiku-4-5",
                effort: EffortLevel::High,
            },
            ModelEffortRefusal::EffortNotAdmitted {
                model: "claude-opus-4-6",
                effort: EffortLevel::XHigh,
                admitted: vec!["low", "medium", "high", "max"],
            },
            ModelEffortRefusal::UnknownModelForPool {
                model: "x".to_owned(),
            },
        ];
        for refusal in bodies {
            let body = refusal.into_error_body();
            assert_eq!(body.code, ErrorCode::InvalidConfig);
            assert!(!body.retryable, "a model refusal is never retryable");
            assert!(
                body.details.get("violation").is_some(),
                "every refusal names its violation"
            );
        }
    }

    /// Every refused tier a refusal MESSAGE names is a tier the interface
    /// accepts.
    ///
    /// The check nothing performed: no test anywhere read any of these strings,
    /// so `"--effort XHigh"` shipped. The assertion is not "the message
    /// contains the right literal" -- that is a second copy of the rendering --
    /// but that the token this sentence puts after `--effort` FEEDS BACK
    /// through the same `EffortLevel` parser the request and the CLI use, and
    /// arrives as the tier that was refused. `XHigh` fails it; `xhigh` passes.
    #[test]
    fn every_token_a_refusal_prints_after_effort_is_one_the_parser_accepts() {
        fn parses_back_as(token: &str) -> Option<EffortLevel> {
            serde_json::from_value(serde_json::Value::String(token.to_owned())).ok()
        }

        // One refusal of every arm that names a tier, for every tier, so the
        // matrix is complete rather than a sample.
        let mut cases: Vec<(EffortLevel, ModelEffortRefusal)> = Vec::new();
        for effort in NAMED_EFFORTS.iter().copied() {
            cases.push((effort, ModelEffortRefusal::EffortWithoutModel { effort }));
            cases.push((
                effort,
                ModelEffortRefusal::UnknownModelWithEffort {
                    model: "claude-invented-9".to_owned(),
                    effort,
                },
            ));
            cases.push((
                effort,
                ModelEffortRefusal::ModelTakesNoEffort {
                    model: "claude-haiku-4-5",
                    effort,
                },
            ));
            cases.push((
                effort,
                ModelEffortRefusal::EffortNotAdmitted {
                    model: "claude-opus-4-6",
                    effort,
                    admitted: EFFORTS_THROUGH_MAX
                        .iter()
                        .map(|admitted| admitted.argv)
                        .collect(),
                },
            ));
        }

        for (effort, refusal) in cases {
            let message = refusal.to_string();
            let (_, after) = message
                .split_once("--effort ")
                .unwrap_or_else(|| panic!("a refusal naming a tier must name the flag: {message}"));
            let token = after
                .split([' ', ';', ',', ':'])
                .next()
                .expect("a token follows the flag");
            assert_eq!(
                parses_back_as(token),
                Some(effort),
                "the message tells the operator to write `--effort {token}`, which is not a value \
                 the interface accepts for {effort:?}: {message}"
            );
        }
    }

    /// The admitted set a refusal offers is offered in accepted spellings too.
    ///
    /// The other half of the same sentence. It used to render with `{:?}` on a
    /// `Vec<&str>`, which quoted every entry -- `it admits ["low", "medium"]` --
    /// so the words an operator would copy carried quotes the shell strips and
    /// clap never sees. Correct by luck; this pins it by rule.
    #[test]
    fn the_admitted_set_a_refusal_offers_is_written_in_accepted_spellings() {
        // 4.6 takes a non-empty ladder that stops short of `xhigh`, which is
        // the only shape that reaches `EffortNotAdmitted` and therefore the
        // only one that offers a set at all.
        let refusal = resolve_model_effort("claude-opus-4-6".into(), Some(EffortLevel::XHigh))
            .expect_err("opus 4.6 predates xhigh");
        let message = refusal.to_string();
        let (_, offered) = message
            .split_once("it admits ")
            .unwrap_or_else(|| panic!("the refusal must offer the admitted set: {message}"));
        let offered: Vec<&str> = offered.split(", ").map(str::trim).collect();
        assert!(!offered.is_empty(), "{message}");
        for token in &offered {
            assert!(
                serde_json::from_value::<EffortLevel>(serde_json::Value::String(
                    (*token).to_owned()
                ))
                .is_ok(),
                "the refusal offers `{token}`, which the interface does not accept: {message}"
            );
        }
        assert!(
            !message.contains('"') && !message.contains('['),
            "the offered set is prose an operator copies, not a Rust literal: {message}"
        );
    }

    /// `Debug` is not a spelling of anything, and the refusals must not use it.
    ///
    /// The regression in one line: `XHigh` is the Rust identifier and no
    /// interface accepts it.
    #[test]
    fn no_refusal_message_carries_the_rust_identifier_for_a_tier() {
        for effort in NAMED_EFFORTS.iter().copied() {
            let debug = format!("{effort:?}");
            for refusal in [
                ModelEffortRefusal::EffortWithoutModel { effort },
                ModelEffortRefusal::ModelTakesNoEffort {
                    model: "claude-haiku-4-5",
                    effort,
                },
            ] {
                let message = refusal.to_string();
                assert!(
                    !message.contains(&debug) || debug == effort.as_str(),
                    "the message spells {effort:?} as its Rust identifier: {message}"
                );
                assert!(
                    message.contains(effort.as_str()),
                    "the message must name the tier in the spelling the flag takes: {message}"
                );
            }
        }
    }

    #[test]
    fn the_table_has_no_ambiguous_spelling() {
        let mut seen: Vec<String> = Vec::new();
        for entry in MODEL_TABLE {
            for spelling in std::iter::once(entry.canonical).chain(entry.aliases.iter().copied()) {
                let lowered = spelling.to_ascii_lowercase();
                assert!(
                    !seen.contains(&lowered),
                    "spelling {spelling} resolves to more than one entry"
                );
                seen.push(lowered);
            }
        }
    }
    /// The model list a refusal offers is the table's own, entry for entry.
    ///
    /// SURVIVING MUTANTS CLOSED: `admitted_model_list -> "xyzzy".into()` and
    /// `-> String::new()` (`class.rs:186`). Its doc already claimed the whole
    /// property -- "derived from `MODEL_TABLE`, so a model added to the table is
    /// offered by every refusal without anyone editing a sentence" -- and
    /// nothing compared the two, so the sentence a refused caller reads could
    /// have been any string at all, including an empty one. This is the same
    /// defect as `agent::supplied_start_paths`, which returned `["xyzzy"]` with
    /// the suite green, in the same shape: an accessor whose doc states a
    /// derivation that no test performs.
    ///
    /// Compared against `MODEL_TABLE` itself rather than a literal, because a
    /// literal here would be the copy the doc says does not exist.
    #[test]
    fn the_offered_model_list_names_every_canonical_model_in_the_table() {
        let offered = admitted_model_list();
        assert!(
            !MODEL_TABLE.is_empty(),
            "an empty model table would make every assertion below vacuous"
        );
        for entry in MODEL_TABLE {
            assert!(
                offered.contains(entry.canonical),
                "the refusal offers {offered:?}, which does not name {:?}",
                entry.canonical
            );
        }
        // And nothing BEYOND the table: split the rendered sentence back apart
        // and require the two sets to be equal, so a hand-added name -- the
        // direction "contains" cannot see -- fails here too.
        let listed: Vec<&str> = offered.split(", ").collect();
        let canonical: Vec<&str> = MODEL_TABLE.iter().map(|entry| entry.canonical).collect();
        assert_eq!(
            listed, canonical,
            "the offered list and the table must be the same models in the same order"
        );
    }
}
