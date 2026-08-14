//! Well-formedness of the one launch-environment policy definition.
//!
//! This replaces two source-text-parsing drift fences —
//! `bin/pmux/tests/launch_environment.rs` and
//! `crates/client/tests/launch_policy_mirror.rs` — that compared hand-kept
//! copies of these tables in `bin/pmux/src/cli.rs` and
//! `crates/client/src/agent_profile.rs` against the daemon's. Both copies are
//! gone, so the thing they guarded cannot happen: there is one definition, and
//! `cargo build` is the fence.
//!
//! What a fence cannot check is whether the surviving definition is *coherent*,
//! and that is what remains worth asserting. An empty prefix would admit every
//! variable in the caller's environment and silently turn the allowlist off; a
//! duplicated or `=`-bearing entry is a typo that the type system is happy with;
//! a prefix that is a prefix of another prefix in the same table means someone
//! misread the table's job. None of those is a compile error, none is visible in
//! review of a 43-entry list, and each is a security defect rather than a
//! cosmetic one.
//!
//! Behavior — case sensitivity, the auth-policy branch, `unknown means denied` —
//! stays pinned where it is enforced, in `crates/service/src/claude_launch.rs`,
//! against the same functions this file exercises. That is deliberate: those
//! tests assert what the launched child receives, which is the property that
//! matters, and duplicating them here would recreate in tests exactly the
//! duplication this consolidation removed from the source.

use pseudomux_protocol::v1::AuthPolicy;
use pseudomux_protocol::v1::launch_environment::{
    INHERITED_EXACT_KEYS, INHERITED_PREFIXES, PROVIDER_ROUTING_EXACT_KEYS,
    PROVIDER_ROUTING_PREFIXES, SUBSCRIPTION_AUTH_KEYS, TRANSPARENT_EXACT_KEYS,
    TRANSPARENT_PREFIXES, inherits, subscription_policy_removes, transparent_profile_removes,
};

/// Every table, named, so a failure says which one.
const EXACT_TABLES: &[(&str, &[&str])] = &[
    ("INHERITED_EXACT_KEYS", INHERITED_EXACT_KEYS),
    ("SUBSCRIPTION_AUTH_KEYS", SUBSCRIPTION_AUTH_KEYS),
    ("PROVIDER_ROUTING_EXACT_KEYS", PROVIDER_ROUTING_EXACT_KEYS),
    ("TRANSPARENT_EXACT_KEYS", TRANSPARENT_EXACT_KEYS),
];

const PREFIX_TABLES: &[(&str, &[&str])] = &[
    ("INHERITED_PREFIXES", INHERITED_PREFIXES),
    ("PROVIDER_ROUTING_PREFIXES", PROVIDER_ROUTING_PREFIXES),
    ("TRANSPARENT_PREFIXES", TRANSPARENT_PREFIXES),
];

#[test]
fn every_policy_table_is_non_empty_and_internally_consistent() {
    for (name, table) in EXACT_TABLES.iter().chain(PREFIX_TABLES) {
        assert!(
            !table.is_empty(),
            "{name} is empty; a policy table that admits or removes nothing is a mistake, \
             not a configuration"
        );
        for entry in *table {
            assert!(
                !entry.is_empty(),
                "{name} contains an empty entry. In an exact table that matches nothing; in a \
                 prefix table `\"\".starts_with()` is true of every name, which would disable \
                 the allowlist entirely."
            );
            assert!(
                !entry.contains('=') && !entry.contains('\0'),
                "{name} entry {entry:?} is not a usable POSIX environment name"
            );
            assert!(
                entry.is_ascii() && !entry.chars().any(char::is_whitespace),
                "{name} entry {entry:?} is not plain ASCII without whitespace"
            );
        }
        let mut sorted = table.to_vec();
        sorted.sort_unstable();
        let unique = {
            let mut unique = sorted.clone();
            unique.dedup();
            unique
        };
        assert_eq!(
            sorted, unique,
            "{name} contains a duplicate entry; the second one is dead text that reviewers will \
             read as a second, differently-justified decision"
        );
    }
}

#[test]
fn no_prefix_is_redundant_against_another_prefix_or_an_exact_name_it_covers() {
    for (name, table) in PREFIX_TABLES {
        for (outer_index, outer) in table.iter().enumerate() {
            for (inner_index, inner) in table.iter().enumerate() {
                assert!(
                    outer_index == inner_index || !inner.starts_with(outer),
                    "{name}: {inner:?} is already covered by the broader prefix {outer:?}. One of \
                     the two is not doing what its comment claims."
                );
            }
        }
    }

    // The allowlist's two branches enumerate exact names only where no admitted
    // prefix in the *same* branch already covers them. A redundant exact name is
    // a reviewer reading a decision that has no effect.
    //
    // `TRANSPARENT_*` is deliberately exempt: `RMUX`, `TMUX`, `TMUX_PANE` and
    // `TMUX_PROGRAM` are named in `TRANSPARENT_EXACT_KEYS` *and* covered by the
    // `RMUX`/`TMUX` prefixes, because that denylist is defense in depth over a
    // set whose exact members are individually load-bearing. Likewise
    // `SUBSCRIPTION_AUTH_KEYS` is intentionally re-covered by `ANTHROPIC_`/`AWS_`
    // under `Inherit`; it is a removal list first and an admission list second.
    for exact in INHERITED_EXACT_KEYS {
        for prefix in INHERITED_PREFIXES {
            assert!(
                !exact.starts_with(prefix),
                "INHERITED_EXACT_KEYS names {exact:?}, which INHERITED_PREFIXES already admits \
                 through {prefix:?}"
            );
        }
    }
    for exact in PROVIDER_ROUTING_EXACT_KEYS {
        for prefix in PROVIDER_ROUTING_PREFIXES {
            assert!(
                !exact.starts_with(prefix),
                "PROVIDER_ROUTING_EXACT_KEYS names {exact:?}, which PROVIDER_ROUTING_PREFIXES \
                 already admits through {prefix:?}"
            );
        }
    }
}

#[test]
fn the_predicates_agree_with_the_tables_they_are_derived_from() {
    // Not a restatement of launch behavior — that is pinned in
    // `crates/service/src/claude_launch.rs`. This asserts only that each
    // exported predicate still reads the table it documents, so a future edit to
    // one cannot leave the other describing a policy nobody enforces.
    for policy in [AuthPolicy::Subscription, AuthPolicy::Inherit] {
        for name in INHERITED_EXACT_KEYS {
            assert!(inherits(name, policy), "{policy:?} denied {name}");
        }
        for prefix in INHERITED_PREFIXES {
            assert!(
                inherits(&format!("{prefix}SUFFIX"), policy),
                "{policy:?} denied a name under {prefix}"
            );
        }
    }

    for name in PROVIDER_ROUTING_EXACT_KEYS {
        assert!(inherits(name, AuthPolicy::Inherit), "inherit denied {name}");
        assert!(
            !inherits(name, AuthPolicy::Subscription),
            "subscription admitted provider routing {name}"
        );
    }
    for prefix in PROVIDER_ROUTING_PREFIXES {
        let name = format!("{prefix}SUFFIX");
        assert!(
            inherits(&name, AuthPolicy::Inherit),
            "inherit denied {name}"
        );
        assert!(
            !inherits(&name, AuthPolicy::Subscription),
            "subscription admitted provider routing {name}"
        );
    }

    for name in SUBSCRIPTION_AUTH_KEYS {
        assert!(
            subscription_policy_removes(name, AuthPolicy::Subscription),
            "subscription failed to remove {name}"
        );
        assert!(
            !subscription_policy_removes(name, AuthPolicy::Inherit),
            "inherit removed {name}, which is the policy it exists to preserve"
        );
    }

    for name in TRANSPARENT_EXACT_KEYS {
        assert!(
            transparent_profile_removes(name),
            "the transparent profile kept {name}"
        );
    }
    for prefix in TRANSPARENT_PREFIXES {
        assert!(
            transparent_profile_removes(&format!("{prefix}SUFFIX")),
            "the transparent profile kept a name under {prefix}"
        );
    }
}
