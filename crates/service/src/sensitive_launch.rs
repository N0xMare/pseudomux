//! Owner-only materialization for launch inputs that must never appear in argv.

use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use pseudomux_protocol::v1::{ClaudeLaunchConfig, ConfigSource, SessionId, SystemPromptPolicy};
use pseudomux_rmux::LaunchSpec;
use tempfile::TempDir;

const MAX_MATERIALIZED_FILE_BYTES: usize = 8 * 1024 * 1024;

/// Keeps sensitive launch files alive for exactly the lifetime of a pmux session.
///
/// The directory is created below pmux's private `0700` runtime directory. Dropping
/// this value removes the random, session-scoped directory and every file in it.
pub struct SensitiveLaunchFiles {
    _directory: TempDir,
    system_prompt: Option<SystemPromptFile>,
}

struct SystemPromptFile {
    flag: &'static str,
    path: PathBuf,
}

impl SensitiveLaunchFiles {
    /// Replaces inline JSON and system-prompt text with owner-only files.
    pub fn prepare(
        runtime_dir: &Path,
        session_id: SessionId,
        config: &mut ClaudeLaunchConfig,
    ) -> Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix(&format!("launch-{session_id}-"))
            .tempdir_in(runtime_dir)
            .context("failed to create private launch material directory")?;
        set_owner_directory_permissions(directory.path())?;

        materialize_sources(directory.path(), "settings", &mut config.settings)?;
        materialize_sources(directory.path(), "mcp", &mut config.mcp_configs)?;

        let system_prompt = match std::mem::take(&mut config.system_prompt) {
            SystemPromptPolicy::Default => None,
            SystemPromptPolicy::Append { prompt } => Some(materialize_prompt(
                directory.path(),
                "append-system-prompt.txt",
                "--append-system-prompt-file",
                &prompt,
            )?),
            SystemPromptPolicy::Replace { prompt } => Some(materialize_prompt(
                directory.path(),
                "system-prompt.txt",
                "--system-prompt-file",
                &prompt,
            )?),
        };

        Ok(Self {
            _directory: directory,
            system_prompt,
        })
    }

    /// Adds only a private pathname to argv after the ordinary launch is resolved.
    pub fn apply_to(&self, launch: &mut LaunchSpec) {
        if let Some(prompt) = &self.system_prompt {
            launch.args.extend([
                prompt.flag.to_owned(),
                prompt.path.to_string_lossy().into_owned(),
            ]);
        }
    }

    #[cfg(test)]
    fn directory_path(&self) -> &Path {
        self._directory.path()
    }
}

impl fmt::Debug for SensitiveLaunchFiles {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SensitiveLaunchFiles")
            .field("file_directory", &"[PRIVATE]")
            .field("has_system_prompt", &self.system_prompt.is_some())
            .finish()
    }
}

fn materialize_sources(directory: &Path, prefix: &str, sources: &mut [ConfigSource]) -> Result<()> {
    for (index, source) in sources.iter_mut().enumerate() {
        let ConfigSource::Inline { document } = source else {
            continue;
        };
        let bytes = serde_json::to_vec(document).context("failed to encode inline JSON config")?;
        let path = directory.join(format!("{prefix}-{index:04}.json"));
        write_private_file(&path, &bytes)?;
        *source = ConfigSource::File {
            path: path.to_string_lossy().into_owned(),
        };
    }
    Ok(())
}

fn materialize_prompt(
    directory: &Path,
    filename: &str,
    flag: &'static str,
    prompt: &str,
) -> Result<SystemPromptFile> {
    if prompt.is_empty() || prompt.contains('\0') {
        bail!("system prompt must be non-empty and contain no NUL");
    }
    let path = directory.join(filename);
    write_private_file(&path, prompt.as_bytes())?;
    Ok(SystemPromptFile { flag, path })
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_MATERIALIZED_FILE_BYTES {
        bail!("sensitive launch material exceeds the {MAX_MATERIALIZED_FILE_BYTES} byte limit");
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to create private launch file at {}", path.display()))?;
    file.write_all(bytes)
        .context("failed to write private launch material")?;
    file.sync_all()
        .context("failed to flush private launch material")?;
    Ok(())
}

fn set_owner_directory_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .context("failed to restrict private launch material directory")?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pseudomux_protocol::v1::SystemPromptPolicy;
    use pseudomux_rmux::EnvironmentSnapshot;
    use serde_json::json;

    fn config(secret: &str) -> ClaudeLaunchConfig {
        ClaudeLaunchConfig {
            executable: "/bin/sh".into(),
            model: None,
            effort: None,
            permission_mode: None,
            allowed_tools: Vec::new(),
            denied_tools: Vec::new(),
            settings: vec![ConfigSource::Inline {
                document: json!({"secret": secret}),
            }],
            mcp_configs: vec![ConfigSource::Inline {
                document: json!({"token": secret}),
            }],
            plugin_dirs: Vec::new(),
            system_prompt: SystemPromptPolicy::Replace {
                prompt: secret.into(),
            },
            extra_args: Vec::new(),
        }
    }

    #[test]
    fn secrets_are_files_not_process_arguments() {
        let runtime = tempfile::tempdir().unwrap();
        let mut config = config("do-not-leak");
        let files =
            SensitiveLaunchFiles::prepare(runtime.path(), SessionId::nil(), &mut config).unwrap();
        let mut launch = LaunchSpec {
            executable: PathBuf::from("/bin/sh"),
            args: Vec::new(),
            cwd: std::env::current_dir().unwrap(),
            environment: EnvironmentSnapshot::default(),
        };
        files.apply_to(&mut launch);

        assert_eq!(config.system_prompt, SystemPromptPolicy::Default);
        assert!(
            config
                .settings
                .iter()
                .all(|source| matches!(source, ConfigSource::File { .. }))
        );
        assert!(
            config
                .mcp_configs
                .iter()
                .all(|source| matches!(source, ConfigSource::File { .. }))
        );
        assert!(!launch.args.iter().any(|arg| arg.contains("do-not-leak")));
        assert_eq!(launch.args[0], "--system-prompt-file");

        #[cfg(unix)]
        for entry in std::fs::read_dir(files.directory_path()).unwrap() {
            use std::os::unix::fs::PermissionsExt;
            let mode = entry.unwrap().metadata().unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn artifacts_are_removed_on_drop() {
        let runtime = tempfile::tempdir().unwrap();
        let mut config = config("ephemeral");
        let path = {
            let files =
                SensitiveLaunchFiles::prepare(runtime.path(), SessionId::nil(), &mut config)
                    .unwrap();
            files.directory_path().to_path_buf()
        };
        assert!(!path.exists());
    }

    #[test]
    fn empty_system_prompt_is_rejected_without_leaking_it() {
        let runtime = tempfile::tempdir().unwrap();
        let mut config = config("");
        let error = SensitiveLaunchFiles::prepare(runtime.path(), SessionId::nil(), &mut config)
            .unwrap_err();
        assert!(error.to_string().contains("non-empty"));
    }
}
