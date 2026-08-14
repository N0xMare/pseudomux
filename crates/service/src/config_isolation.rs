//! Seeding for a pmux-owned, private Claude configuration root.
//!
//! # Why this exists
//!
//! Without a private root, every Path B cell writes its transcripts into the
//! operator's `~/.claude/projects` and its trust record into the machine-wide
//! `~/.claude.json`. `CLAUDE_CONFIG_DIR` alone does not buy the separation,
//! because Claude namespaces the macOS keychain SERVICE NAME by
//! `sha256(config_dir)[0..8]`, so a fresh root looks up an empty item and
//! reports "Not logged in" -- which is exactly why `docs/path-b.md` §2.2
//! recorded a `CLAUDE_CONFIG_DIR` override as REJECTED. The missing half is
//! `CLAUDE_SECURESTORAGE_CONFIG_DIR`, which decouples the credential store from
//! the config dir and is delivered by
//! `claude_launch.rs::build_environment` step 6. This module owns the other
//! half: making a root Claude will actually start in without a dialog.
//!
//! # What a fresh root needs, and why each item is here
//!
//! Every claim below is pinned to the compiled bundle at
//! `~/.local/share/claude/versions/2.1.220` (`claude --version` ->
//! `2.1.220 (Claude Code)`), the same version every other `/clear` fact in
//! `docs/path-b.md` is pinned to.
//!
//! * **`hasCompletedOnboarding: true`** -- required. The gate is
//!   `let l=Rt(),c=!1;if(!l.hasCompletedOnboarding||...){c=!0;...Onboarding...}`
//!   where `Rt()` reads this file. Without it turn 1 hangs on an onboarding
//!   screen for its full deadline.
//! * **`projects.<canonical cwd>.hasTrustDialogAccepted: true`** -- required.
//!   `J3y()` checks `e.projects?.[fbe()]?.hasTrustDialogAccepted` and then
//!   walks parents from `VMe(xt())` to the filesystem root. `xt()` is the
//!   process cwd and `VMe` is `path.normalize`, and pmux launches the child
//!   with a `Path::canonicalize`d cwd, so the canonical cwd matches on the
//!   FIRST iteration of that walk. That is why exactly one key is written and
//!   why it does not matter that `fbe()`'s own spelling routes through a
//!   git-root indirection this crate cannot resolve.
//! * **`settings.json` `env.DISABLE_AUTOUPDATER: "1"`** -- not required for
//!   correctness, but an in-session updater that changes the binary
//!   mid-campaign invalidates the compatibility cell's version key. Written
//!   HERE and not as `.claude.json`'s `autoUpdates: false`, which is the
//!   spelling Claude's own private-root recipe uses and which is TRANSIENT.
//!   `aCm()` reads
//!   `if(e.autoUpdates!==!1||e.autoUpdatesProtectedForNative===!0)return!0`,
//!   so the migration fires on precisely the value a seed would assert, writes
//!   `env:{DISABLE_AUTOUPDATER:"1"}` into userSettings, and then deletes both
//!   keys via `await hr((n)=>{let{autoUpdates:o,autoUpdatesProtectedForNative:i,
//!   ...s}=n;return s})`. MEASURED on a real launch: one `claude -p` against a
//!   root seeded the old way left `.claude.json` with no `autoUpdates` key and
//!   `settings.json` holding exactly `{"env":{"DISABLE_AUTOUPDATER":"1"}}`, so
//!   every later start read-modify-wrote the file and every
//!   [`SeedDisposition::VerifyOnly`] start refused. Seeding the migration's own
//!   destination is stable because `aCm()` then returns at its first
//!   condition. See `a_real_claude_launch_leaves_the_seed_already_satisfied`.
//! * **`bypassPermissionsModeAccepted: false`** -- from the same recipe, and
//!   deliberately `false` even when the caller asked for
//!   `--dangerously-skip-permissions`. See [`Self::user_settings_document`].
//!   Stable, unlike its neighbour: `cCm()` opens
//!   `if(!Rt().bypassPermissionsModeAccepted)return!0`, so a falsy value is a
//!   no-op. The two keys sat one line apart in Claude's recipe and only one of
//!   them can be asserted; that is why every key this module writes is pinned
//!   to a real launch and not only to a second call of this function.
//!
//! # The race pmux cannot win, and the rule that avoids it
//!
//! Claude writes this same file itself, under its own lock, with its own
//! stale-write telemetry and an auto-repair path. pmux does not implement that
//! protocol. So pmux writes only when **no live session is bound to the root**
//! ([`SeedDisposition`]); when one is, it performs a read-only check and
//! refuses the start rather than racing. Seeds within one daemon are already
//! serialized by `NativeService::start_guard`, which is held across the whole
//! of `start_session_internal`, so no second lock is introduced here.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

/// The file `Rt()` reads: `${CLAUDE_CONFIG_DIR || homedir()}/.claude.json` for
/// the production OAuth environment (`XUn()` returns `""` there).
const GLOBAL_CONFIG_FILE: &str = ".claude.json";

/// `lE()` prefers `<config_dir>/.config.json` over `.claude.json` whenever it
/// exists. A root carrying one would make everything this module writes inert,
/// and the failure would present as a turn-1 hang rather than as a bad seed.
const SHADOWING_CONFIG_FILE: &str = ".config.json";

/// `mnt("userSettings", …)` resolves to `path.join(path.resolve(fn()),
/// "settings.json")`, where `fn()` is the config dir. This is therefore the
/// file `--setting-sources user` reads, and the file `PW()` consults for
/// `skipDangerousModePermissionPrompt`.
const USER_SETTINGS_FILE: &str = "settings.json";

/// The environment name inside `settings.json` that turns the background
/// updater off, and the value Claude's own migration writes.
const DISABLE_AUTOUPDATER: &str = "DISABLE_AUTOUPDATER";
const DISABLE_AUTOUPDATER_VALUE: &str = "1";

/// Whether pmux is allowed to write to this root right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SeedDisposition {
    /// No live session is bound to the root: pmux owns the files and may write.
    Write,
    /// A live Claude process is bound to the root. pmux checks read-only and
    /// refuses the start if the required state is absent, because the only
    /// alternative is a concurrent read-modify-write against a writer whose
    /// locking protocol pmux does not implement.
    VerifyOnly,
}

/// What [`seed_private_config_root`] actually did, so a caller (and a test) can
/// tell "already correct" from "repaired".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SeedOutcome {
    AlreadySeeded,
    Wrote,
}

/// The exact state one session needs a private root to be in.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConfigRootSeed<'a> {
    /// Owner-only directory pmux owns, already canonicalized and validated by
    /// `claude_launch::validate_config_isolation`.
    pub root: &'a Path,
    /// The canonical cwd the child will be launched with -- the trust key.
    pub trusted_cwd: &'a Path,
    /// True when argv carries `--dangerously-skip-permissions`.
    pub dangerous_permission_bypass: bool,
}

/// Refuses a root whose `.config.json` would shadow the file pmux seeds.
///
/// Lives here rather than in `claude_launch.rs` because the reason is a fact
/// about Claude's config resolution, but it is called from admission so the
/// refusal happens before any side effect.
pub(crate) fn refuse_shadowed_config_file(root: &Path) -> Result<()> {
    let shadow = root.join(SHADOWING_CONFIG_FILE);
    if shadow.exists() {
        bail!(
            "config isolation root contains {SHADOWING_CONFIG_FILE}, which Claude reads in preference to {GLOBAL_CONFIG_FILE} and would make the pmux seed inert: {}",
            shadow.display()
        );
    }
    Ok(())
}

impl ConfigRootSeed<'_> {
    /// The `.claude.json` pmux asserts, merged over whatever is already there.
    ///
    /// Merge semantics are deliberately narrow: pmux sets exactly the keys it
    /// names and touches nothing else, including other projects' trust records
    /// and every key Claude writes for itself.
    fn global_config_document(&self, existing: Map<String, Value>) -> Result<Map<String, Value>> {
        let mut document = existing;
        document.insert("hasCompletedOnboarding".into(), Value::Bool(true));
        // No `autoUpdates` key. Asserting `false` here is what TRIGGERS
        // `aCm()`, which migrates the preference into userSettings and deletes
        // this key; the durable half is written by `user_settings_document`.
        // Always `false`, never mirroring `dangerous_permission_bypass`.
        //
        // `true` DOES suppress the modal -- `GyT` returns early on
        // `PW()||Rt().bypassPermissionsModeAccepted` -- but it is a TRANSIENT
        // value: `cCm()` migrates it on startup by writing
        // `skipDangerousModePermissionPrompt:true` into userSettings and then
        // DELETING this key. A seed that asserted `true` would be erased by the
        // first launch, would be rewritten by every subsequent start, and under
        // `SeedDisposition::VerifyOnly` would REFUSE a root whose live session
        // had merely done what Claude always does. pmux therefore writes the
        // durable half directly; see `user_settings_document`.
        document.insert("bypassPermissionsModeAccepted".into(), Value::Bool(false));

        let key = trust_key(self.trusted_cwd)?;
        let projects = document
            .entry("projects".to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(projects) = projects.as_object_mut() else {
            bail!("existing {GLOBAL_CONFIG_FILE} has a non-object `projects` member");
        };
        let project = projects
            .entry(key)
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(project) = project.as_object_mut() else {
            bail!("existing {GLOBAL_CONFIG_FILE} has a non-object project record for the cwd");
        };
        project.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
        Ok(document)
    }

    /// The `settings.json` pmux asserts.
    ///
    /// This file, and not `.claude.json`, is where every DURABLE preference
    /// lives. Claude's startup migrations move preferences in exactly this
    /// direction and delete the originals, so a key asserted on the far side of
    /// a migration is stable and one asserted on the near side is erased by the
    /// first launch.
    ///
    /// `env.DISABLE_AUTOUPDATER` is always present. It is the exact destination
    /// `aCm()` writes when it retires `.claude.json`'s `autoUpdates: false`, and
    /// it is read by `Q$e()`, whose second clause is
    /// `if(Z.DISABLE_AUTOUPDATER)return{type:"env",envVar:"DISABLE_AUTOUPDATER"}`
    /// -- `Z` being the environment view that includes this settings block, as
    /// Claude's own operator-facing text states: "`DISABLE_AUTOUPDATER` is set
    /// -- including via the `env` block of the user's own
    /// `~/.claude/settings.json`, where the legacy `autoUpdates: false`
    /// preference gets migrated".
    ///
    /// When the caller asked for `--dangerously-skip-permissions` it also
    /// carries `skipDangerousModePermissionPrompt: true`, which is the key
    /// `PW()` reads and the destination `cCm()` writes to. The alternative is a
    /// guaranteed turn-1 modal with no answering surface. This is not pmux
    /// inventing consent: the caller already declared the intent explicitly,
    /// and every turn of such a session already carries the
    /// `dangerous_permission_bypass` warning.
    ///
    /// The `env` block is MERGED rather than replaced. pmux owns one name in it;
    /// an operator or a Claude migration that put another there keeps it, which
    /// is the same narrow merge rule the global document already follows.
    fn user_settings_document(&self, existing: Map<String, Value>) -> Result<Map<String, Value>> {
        let mut document = existing;
        let environment = document
            .entry("env".to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(environment) = environment.as_object_mut() else {
            bail!("existing {USER_SETTINGS_FILE} has a non-object `env` member");
        };
        environment.insert(
            DISABLE_AUTOUPDATER.into(),
            Value::String(DISABLE_AUTOUPDATER_VALUE.into()),
        );
        if self.dangerous_permission_bypass {
            document.insert(
                "skipDangerousModePermissionPrompt".into(),
                Value::Bool(true),
            );
        }
        Ok(document)
    }
}

/// Everything a root pmux is about to bind a minified cell to may contain.
///
/// Exactly the two files this module writes. Anything else is evidence the root
/// has been used, and a used root is a root carrying a previous caller's bytes.
const PRISTINE_ROOT_ENTRIES: [&str; 2] = [GLOBAL_CONFIG_FILE, USER_SETTINGS_FILE];

/// Refuses a config root that has served before, for the one cell that claims it
/// carries nothing from the caller before it.
///
/// `assert_empty_after_clear` reads ONE file -- the bound transcript -- and can
/// say nothing about the rest of the root. The root is where the rest of a
/// session's residue lives, and MEASURED on Claude Code 2.1.220 it is
/// substantially per-ROOT rather than per-session:
///
/// * `history.jsonl` -- one file per root, each row tagged with a `project`
///   (cwd) and a `sessionId`. Append-only under a lock; `/clear` does not
///   truncate it and is itself appended as a row. Composer recall filters on
///   `project` by default, so it SPANS every rotation, and `"everywhere"` is a
///   settings key away. MEASURED on this host: 1,556 rows, 49 distinct
///   projects, one file; a 77-session probe directory contributed 146 rows of
///   which 65 were `/clear` itself.
/// * `paste-cache/` -- content-addressed, not project-scoped, mode 0600. Any
///   pasted content over 1,024 characters is stored here verbatim instead of
///   inline, and pmux injects every prompt by bracketed paste. Cleanup is
///   mtime-based, so it outlives transcript pruning.
/// * `projects/` -- every transcript, including the abandoned ones each `/clear`
///   leaves behind.
/// * `backups/` -- up to five rotating `.claude.json` snapshots, each carrying
///   the whole projects map: every cwd and every `lastSessionId`.
/// * `shell-snapshots/`, `sessions/`, `debug/`, `cache/` -- per-root, and
///   Claude's own cleanup code says of the first, verbatim, that it is "not
///   project-scoped and will not be touched".
///
/// A per-cell root closes all of these at once, which is why the rule is stated
/// once about the root rather than once per channel: the list is Claude's to
/// extend, so an allowlist of two names is the only form of it that stays true.
pub(crate) fn require_pristine_root_for_minified_cell(root: &Path) -> Result<()> {
    let entries = std::fs::read_dir(root).with_context(|| {
        format!(
            "failed to inspect config isolation root: {}",
            root.display()
        )
    })?;
    for entry in entries {
        let entry = entry.with_context(|| {
            format!(
                "failed to inspect config isolation root: {}",
                root.display()
            )
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if PRISTINE_ROOT_ENTRIES.contains(&name.as_ref()) {
            continue;
        }
        bail!(
            "a minified cell requires a config isolation root it alone has ever used, and this one contains {name}: {}",
            root.display()
        );
    }
    Ok(())
}

/// Brings one private config root to the state a session needs, or refuses.
pub(crate) fn seed_private_config_root(
    seed: &ConfigRootSeed<'_>,
    disposition: SeedDisposition,
) -> Result<SeedOutcome> {
    refuse_shadowed_config_file(seed.root)?;

    // The global config is reconciled first. It carries both hard requirements
    // (onboarding, trust), so a root that fails for a reason the operator has
    // to fix reports that reason before anything is written at all.
    let global = reconcile(
        &seed.root.join(GLOBAL_CONFIG_FILE),
        disposition,
        |existing| seed.global_config_document(existing),
    )?;
    let settings = reconcile(
        &seed.root.join(USER_SETTINGS_FILE),
        disposition,
        |existing| seed.user_settings_document(existing),
    )?;
    // `<root>/projects/` is deliberately NOT created. `TranscriptLocator`
    // guards its scan with `projects_root.is_dir()` and its fast path just
    // yields non-existent candidates, so a missing directory is an empty
    // collision set -- which is what a fresh root should mean. Claude creates
    // it on first launch.
    Ok(match (global, settings) {
        (SeedOutcome::AlreadySeeded, SeedOutcome::AlreadySeeded) => SeedOutcome::AlreadySeeded,
        _ => SeedOutcome::Wrote,
    })
}

/// Read, compare, and write one file only if the comparison fails.
///
/// The "already correct" short circuit is what makes seeding idempotent and
/// what lets a root hosting a live session be admitted without a write.
fn reconcile(
    path: &Path,
    disposition: SeedDisposition,
    desired: impl FnOnce(Map<String, Value>) -> Result<Map<String, Value>>,
) -> Result<SeedOutcome> {
    let existing = read_json_object_without_following(path)?;
    let current = existing.clone().unwrap_or_default();
    let wanted = desired(current.clone())?;
    if existing.is_some() && wanted == current {
        return Ok(SeedOutcome::AlreadySeeded);
    }
    if disposition == SeedDisposition::VerifyOnly {
        bail!(
            "config isolation root needs seeding but is in use by a live session: {}",
            path.display()
        );
    }
    write_private_json_atomically(path, &Value::Object(wanted))?;
    Ok(SeedOutcome::Wrote)
}

/// The `projects` key Claude looks the cwd up under.
///
/// `VMe(e)` is `path.normalize(e)` on unix, and pmux hands the child an
/// already-canonical absolute path, so the byte string is the key. No NFC
/// normalization is applied: `fbe()`/`Uon()` do not apply any either -- only
/// the config-dir and securestorage-dir helpers do.
fn trust_key(cwd: &Path) -> Result<String> {
    cwd.to_str()
        .map(str::to_owned)
        .with_context(|| format!("cwd is not UTF-8: {}", cwd.display()))
}

/// Reads a JSON object, refusing to traverse a symlink at the final component.
///
/// A symlinked `.claude.json` inside a root pmux claims to own is a refusal,
/// not a follow: following it would let anyone who can create that name inside
/// the root redirect pmux's write to a file it was never given.
fn read_json_object_without_following(path: &Path) -> Result<Option<Map<String, Value>>> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            #[cfg(unix)]
            if error.raw_os_error() == Some(libc::ELOOP) {
                bail!(
                    "{} is a symlink; pmux refuses to write through it",
                    path.display()
                );
            }
            return Err(error.into());
        }
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(Some(Map::new()));
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(object)) => Ok(Some(object)),
        Ok(_) => bail!("{} does not contain a JSON object", path.display()),
        Err(error) => bail!("{} is not valid JSON: {error}", path.display()),
    }
}

/// Owner-only, crash-safe replacement of one file inside the private root.
///
/// Temp file in the same directory with `create_new` + mode `0600`, `write_all`,
/// `sync_all`, `rename`, then an fsync of the directory -- so a crash mid-write
/// cannot leave a truncated `.claude.json`, which is precisely the failure
/// `docs/path-b.md` §5 mandates against for this file.
fn write_private_json_atomically(path: &Path, value: &Value) -> Result<()> {
    let directory = path
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no file name", path.display()))?;
    let bytes = serde_json::to_vec_pretty(value).context("failed to encode private config")?;

    let temporary = unique_temporary_path(directory, file_name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("failed to create {}", temporary.display()))?;
    let staged = TemporaryFileGuard(temporary);
    file.write_all(&bytes)
        .context("failed to write private config")?;
    file.sync_all().context("failed to flush private config")?;
    drop(file);
    std::fs::rename(&staged.0, path)
        .with_context(|| format!("failed to publish {}", path.display()))?;
    std::mem::forget(staged);
    sync_directory(directory)
}

/// Removes the staged temp file if publication never happens.
struct TemporaryFileGuard(PathBuf);

impl Drop for TemporaryFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn unique_temporary_path(directory: &Path, file_name: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
    directory.join(format!(
        ".{file_name}.pmux-{}-{nonce}.tmp",
        std::process::id()
    ))
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<()> {
    std::fs::File::open(directory)
        .and_then(|handle| handle.sync_all())
        .with_context(|| format!("failed to fsync {}", directory.display()))
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    /// The keys pmux asserts, named in one place so a test cannot drift from
    /// the seed by asserting a subset of it.
    fn asserted_global_keys() -> [(&'static str, Value); 2] {
        [
            ("hasCompletedOnboarding", Value::Bool(true)),
            ("bypassPermissionsModeAccepted", Value::Bool(false)),
        ]
    }

    /// The keys pmux asserts in `settings.json` for an ordinary session.
    fn asserted_settings() -> Value {
        json!({"env": {DISABLE_AUTOUPDATER: DISABLE_AUTOUPDATER_VALUE}})
    }

    fn root() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    fn seed<'a>(root: &'a Path, cwd: &'a Path) -> ConfigRootSeed<'a> {
        ConfigRootSeed {
            root,
            trusted_cwd: cwd,
            dangerous_permission_bypass: false,
        }
    }

    fn read(path: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn a_fresh_root_is_seeded_with_onboarding_trust_and_no_projects_directory() {
        let directory = root();
        let cwd = tempfile::tempdir().unwrap();
        let outcome =
            seed_private_config_root(&seed(directory.path(), cwd.path()), SeedDisposition::Write)
                .unwrap();
        assert_eq!(outcome, SeedOutcome::Wrote);

        let config = read(&directory.path().join(GLOBAL_CONFIG_FILE));
        for (key, value) in asserted_global_keys() {
            assert_eq!(config.get(key), Some(&value), "{key}");
        }
        assert_eq!(
            config
                .pointer(&format!(
                    "/projects/{}/hasTrustDialogAccepted",
                    cwd.path()
                        .to_str()
                        .unwrap()
                        .replace('~', "~0")
                        .replace('/', "~1")
                ))
                .and_then(Value::as_bool),
            Some(true),
            "the canonical cwd must be the trust key: {config}"
        );
        assert!(
            config.get("autoUpdates").is_none(),
            "asserting autoUpdates is what triggers the migration that deletes it: {config}"
        );
        assert_eq!(
            read(&directory.path().join(USER_SETTINGS_FILE)),
            asserted_settings(),
            "the updater preference belongs where Claude's own migration puts it"
        );
        assert!(
            !directory.path().join("projects").exists(),
            "a fresh root must not pre-create projects/, so the locator sees an empty collision set"
        );

        let mode = std::fs::metadata(directory.path().join(GLOBAL_CONFIG_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the seed must be owner-only");
    }

    /// Idempotence against pmux, which is the weaker of the two claims.
    ///
    /// Calling the seeder twice with no Claude in between proves only that this
    /// function is deterministic. The claim that matters -- that the seed
    /// survives a launch -- is not observable here at all, which is how
    /// `autoUpdates: false` passed this test while being deleted by the first
    /// real launch. See
    /// `a_real_claude_launch_leaves_the_seed_already_satisfied`.
    #[test]
    fn seeding_is_idempotent_and_a_satisfied_root_is_not_rewritten() {
        let directory = root();
        let cwd = tempfile::tempdir().unwrap();
        let request = seed(directory.path(), cwd.path());
        assert_eq!(
            seed_private_config_root(&request, SeedDisposition::Write).unwrap(),
            SeedOutcome::Wrote
        );
        assert_eq!(
            seed_private_config_root(&request, SeedDisposition::Write).unwrap(),
            SeedOutcome::AlreadySeeded
        );
        // The whole point of the read-only disposition: an already-correct root
        // in use by a live session is admitted without a write.
        assert_eq!(
            seed_private_config_root(&request, SeedDisposition::VerifyOnly).unwrap(),
            SeedOutcome::AlreadySeeded
        );
    }

    #[test]
    fn a_root_in_use_by_a_live_session_is_never_written_to() {
        let directory = root();
        let cwd = tempfile::tempdir().unwrap();
        let error = seed_private_config_root(
            &seed(directory.path(), cwd.path()),
            SeedDisposition::VerifyOnly,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("in use by a live session"),
            "unexpected refusal: {error}"
        );
        assert!(
            !directory.path().join(GLOBAL_CONFIG_FILE).exists(),
            "a refusal must not leave a partially seeded root behind"
        );
    }

    #[test]
    fn a_new_cwd_under_a_live_root_is_refused_rather_than_raced() {
        let directory = root();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        seed_private_config_root(
            &seed(directory.path(), first.path()),
            SeedDisposition::Write,
        )
        .unwrap();
        let before = std::fs::read(directory.path().join(GLOBAL_CONFIG_FILE)).unwrap();
        let error = seed_private_config_root(
            &seed(directory.path(), second.path()),
            SeedDisposition::VerifyOnly,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("in use by a live session"),
            "unexpected refusal: {error}"
        );
        assert_eq!(
            std::fs::read(directory.path().join(GLOBAL_CONFIG_FILE)).unwrap(),
            before,
            "pmux must not read-modify-write a file a live Claude owns"
        );
    }

    #[test]
    fn foreign_keys_and_other_projects_survive_a_reseed() {
        let directory = root();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(GLOBAL_CONFIG_FILE),
            serde_json::to_vec(&json!({
                "oauthAccount": {"accountUuid": "keep-me"},
                "projects": {"/some/other/project": {"hasTrustDialogAccepted": true}}
            }))
            .unwrap(),
        )
        .unwrap();
        seed_private_config_root(&seed(directory.path(), cwd.path()), SeedDisposition::Write)
            .unwrap();
        let config = read(&directory.path().join(GLOBAL_CONFIG_FILE));
        assert_eq!(
            config.pointer("/oauthAccount/accountUuid"),
            Some(&json!("keep-me")),
            "the seed must touch no key it did not write"
        );
        assert_eq!(
            config.pointer("/projects/~1some~1other~1project/hasTrustDialogAccepted"),
            Some(&json!(true))
        );
    }

    #[test]
    fn a_symlinked_config_file_is_refused_rather_than_followed() {
        let directory = root();
        let cwd = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let victim = elsewhere.path().join("victim.json");
        std::fs::write(&victim, b"{\"untouched\":true}").unwrap();
        std::os::unix::fs::symlink(&victim, directory.path().join(GLOBAL_CONFIG_FILE)).unwrap();

        let error =
            seed_private_config_root(&seed(directory.path(), cwd.path()), SeedDisposition::Write)
                .unwrap_err()
                .to_string();
        assert!(
            format!("{error:?}").contains("symlink") || error.contains("symlink"),
            "unexpected refusal: {error}"
        );
        assert_eq!(
            std::fs::read(&victim).unwrap(),
            b"{\"untouched\":true}",
            "pmux must not write through a symlink planted in its own root"
        );
    }

    #[test]
    fn a_shadowing_config_json_is_refused_because_the_seed_would_be_inert() {
        let directory = root();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(SHADOWING_CONFIG_FILE), b"{}").unwrap();
        let error =
            seed_private_config_root(&seed(directory.path(), cwd.path()), SeedDisposition::Write)
                .unwrap_err()
                .to_string();
        assert!(
            error.contains(SHADOWING_CONFIG_FILE),
            "unexpected refusal: {error}"
        );
        assert!(!directory.path().join(GLOBAL_CONFIG_FILE).exists());
    }

    #[test]
    fn a_dangerous_bypass_request_accepts_the_dialog_where_claude_reads_it() {
        let directory = root();
        let cwd = tempfile::tempdir().unwrap();
        let request = ConfigRootSeed {
            root: directory.path(),
            trusted_cwd: cwd.path(),
            dangerous_permission_bypass: true,
        };
        seed_private_config_root(&request, SeedDisposition::Write).unwrap();
        assert_eq!(
            read(&directory.path().join(USER_SETTINGS_FILE)),
            json!({
                "env": {DISABLE_AUTOUPDATER: DISABLE_AUTOUPDATER_VALUE},
                "skipDangerousModePermissionPrompt": true,
            }),
            "PW() reads userSettings, which resolves to <config root>/settings.json"
        );
        // `cCm()` deletes this key after migrating it, so asserting `true` here
        // would make the seed self-invalidating on first launch.
        assert_eq!(
            read(&directory.path().join(GLOBAL_CONFIG_FILE)).get("bypassPermissionsModeAccepted"),
            Some(&json!(false))
        );
        assert_eq!(
            seed_private_config_root(&request, SeedDisposition::Write).unwrap(),
            SeedOutcome::AlreadySeeded,
            "the bypass seed must be stable across restarts, not rewritten every start"
        );
    }

    #[test]
    fn a_corrupt_config_file_is_refused_rather_than_overwritten() {
        let directory = root();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(GLOBAL_CONFIG_FILE), b"{not json").unwrap();
        let error =
            seed_private_config_root(&seed(directory.path(), cwd.path()), SeedDisposition::Write)
                .unwrap_err()
                .to_string();
        assert!(
            error.contains("not valid JSON"),
            "unexpected refusal: {error}"
        );
        assert_eq!(
            std::fs::read(directory.path().join(GLOBAL_CONFIG_FILE)).unwrap(),
            b"{not json",
            "an unparseable config is a refusal, never a silent replacement"
        );
    }

    #[test]
    fn a_freshly_seeded_root_is_pristine_enough_for_a_minified_cell() {
        let directory = root();
        let cwd = tempfile::tempdir().unwrap();
        seed_private_config_root(&seed(directory.path(), cwd.path()), SeedDisposition::Write)
            .unwrap();
        require_pristine_root_for_minified_cell(directory.path())
            .expect("the seeder's own output must satisfy the rule it is seeded for");
    }

    /// Every per-ROOT residue channel, refused by name.
    ///
    /// The list is deliberately not the point -- the rule is an allowlist of the
    /// two files pmux writes, so a channel Claude adds later is refused without
    /// anyone naming it. These cases exist so the refusal message is proven to
    /// name the offending entry, which is the difference between an operator
    /// deleting one file and an operator deleting a root.
    #[test]
    fn a_root_that_has_served_before_cannot_back_a_minified_cell() {
        for residue in [
            "history.jsonl",
            "paste-cache",
            "projects",
            "backups",
            "shell-snapshots",
            "sessions",
            "statsig",
        ] {
            let directory = root();
            let cwd = tempfile::tempdir().unwrap();
            seed_private_config_root(&seed(directory.path(), cwd.path()), SeedDisposition::Write)
                .unwrap();
            let path = directory.path().join(residue);
            if residue.contains('.') {
                std::fs::write(&path, b"{}\n").unwrap();
            } else {
                std::fs::create_dir(&path).unwrap();
            }
            let error = require_pristine_root_for_minified_cell(directory.path())
                .unwrap_err()
                .to_string();
            assert!(
                error.contains(residue),
                "the refusal must name what made the root unusable: {error}"
            );
        }
    }

    /// The claim the unit tests above CANNOT make: that the seed survives the
    /// program it is written for.
    ///
    /// Every other test in this module calls the seeder twice with no Claude in
    /// between, which proves stability against pmux and says nothing about
    /// stability against Claude. `autoUpdates: false` passed all of them and was
    /// deleted by the first real launch, because writing it is precisely what
    /// fires `aCm()`. So this test runs a real `claude` against a freshly seeded
    /// private root and then re-seeds under [`SeedDisposition::VerifyOnly`] --
    /// the disposition a second start would use -- which fails unless every key
    /// pmux asserted is still exactly as it left it.
    ///
    /// `VerifyOnly` is the assertion rather than a hand-written key list on
    /// purpose: it is the code path a real second start takes, so a key that
    /// becomes transient later fails here for the same reason it would fail in
    /// production, without anyone having to have predicted which key.
    ///
    /// The launch does not need to succeed as a QUERY. Claude runs its startup
    /// migrations before it reaches the model, so a refusal for rate limits or
    /// for missing credentials still exercises everything this test is about;
    /// only a binary that never started would make it vacuous, and that shows up
    /// as the spawn failing.
    ///
    /// `CLAUDE_SECURESTORAGE_CONFIG_DIR` is pinned to the empty string, which is
    /// exactly what `build_environment` step 6 delivers for a caller with no
    /// `CLAUDE_CONFIG_DIR` of its own: it selects the default credential store,
    /// so the isolated root does not look logged-out.
    #[test]
    #[ignore = "runs the operator's real `claude` binary once against a private config root; no credentials are written and no turn is required"]
    fn a_real_claude_launch_leaves_the_seed_already_satisfied() {
        let directory = root();
        let cwd = root();
        let cwd = cwd.path().canonicalize().unwrap();
        let request = ConfigRootSeed {
            root: directory.path(),
            trusted_cwd: &cwd,
            dangerous_permission_bypass: false,
        };
        assert_eq!(
            seed_private_config_root(&request, SeedDisposition::Write).unwrap(),
            SeedOutcome::Wrote
        );

        let status = std::process::Command::new("claude")
            .arg("-p")
            .arg("respond with the single word OK")
            .current_dir(&cwd)
            .env("CLAUDE_CONFIG_DIR", directory.path())
            .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", "")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("the operator's `claude` binary must be on PATH for this test");
        // Deliberately not asserted: a rate-limited or unauthenticated run still
        // ran every startup migration, which is the whole subject here.
        let _ = status;

        assert!(
            directory.path().join("projects").exists()
                || directory.path().join("sessions").exists()
                || directory.path().join("backups").exists(),
            "claude does not appear to have started against the private root at all, \
             which would make this test vacuous"
        );
        assert_eq!(
            seed_private_config_root(&request, SeedDisposition::VerifyOnly).unwrap(),
            SeedOutcome::AlreadySeeded,
            "a key this seed asserts did not survive one real launch: re-seeding after a \
             launch must be a no-op, or every start read-modify-writes a file Claude owns \
             and every VerifyOnly start refuses"
        );

        // And the root is now visibly used, which is the same fact from the
        // other side: it may no longer back a minified cell.
        require_pristine_root_for_minified_cell(directory.path())
            .expect_err("a root a real Claude has started in is no longer pristine");
    }
}
