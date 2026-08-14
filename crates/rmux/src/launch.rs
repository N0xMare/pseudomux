use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const LAUNCHER_PROTOCOL_VERSION: u16 = 1;
pub const MAX_LAUNCHER_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// One-use capability presented by `pmux-launcher` to pmuxd.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LaunchToken(String);

impl LaunchToken {
    /// Generates a new unpredictable capability.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::new_v4().simple().to_string())
    }

    /// Parses a token received over the private launcher command line.
    pub fn parse(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        let parsed = Uuid::parse_str(&value).map_err(|_| "launch token is not a UUID")?;
        Ok(Self(parsed.simple().to_string()))
    }

    /// Exposes the opaque token only for the one-shot launcher handshake.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for LaunchToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LaunchToken([REDACTED])")
    }
}

/// Complete environment selected for the foreground Claude process.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentSnapshot {
    /// Exact name/value map passed to the replacement process.
    pub variables: BTreeMap<String, String>,
}

impl fmt::Debug for EnvironmentSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnvironmentSnapshot")
            .field("variable_count", &self.variables.len())
            .finish_non_exhaustive()
    }
}

impl EnvironmentSnapshot {
    /// Captures the current process environment without logging it.
    #[must_use]
    pub fn capture() -> Self {
        Self {
            variables: std::env::vars().collect(),
        }
    }

    /// Applies explicit set and unset operations.
    #[must_use]
    pub fn patched(
        mut self,
        set: impl IntoIterator<Item = (String, String)>,
        unset: impl IntoIterator<Item = String>,
    ) -> Self {
        for key in unset {
            self.variables.remove(&key);
        }
        for (key, value) in set {
            self.variables.insert(key, value);
        }
        self
    }
}

/// Process specification returned exactly once to `pmux-launcher`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchSpec {
    /// Absolute Claude executable path.
    pub executable: PathBuf,
    /// Arguments excluding `argv[0]`. Prompts are forbidden here.
    pub args: Vec<String>,
    /// Claude workspace.
    pub cwd: PathBuf,
    /// Exact replacement environment.
    pub environment: EnvironmentSnapshot,
}

impl fmt::Debug for LaunchSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LaunchSpec")
            .field("executable", &self.executable)
            .field("argument_count", &self.args.len())
            .field("cwd", &self.cwd)
            .field("environment", &self.environment)
            .finish()
    }
}

impl LaunchSpec {
    /// Validates properties required before the launcher may call exec.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.executable.is_absolute() {
            return Err("Claude executable must be absolute");
        }
        if !self.cwd.is_absolute() {
            return Err("Claude cwd must be absolute");
        }
        if self.args.iter().any(|arg| arg.contains('\0')) {
            return Err("Claude arguments may not contain NUL");
        }
        if self.environment.variables.iter().any(|(key, value)| {
            key.is_empty() || key.contains('=') || key.contains('\0') || value.contains('\0')
        }) {
            return Err("invalid process environment");
        }
        Ok(())
    }
}

/// One-shot request sent by the launcher. The token must never be logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LauncherRequest {
    pub version: u16,
    pub token: LaunchToken,
}

impl fmt::Debug for LauncherRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LauncherRequest")
            .field("version", &self.version)
            .field("token", &self.token)
            .finish()
    }
}

/// One-shot broker response. Callers must not log the successful variant.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LauncherResponse {
    Ready { version: u16, spec: LaunchSpec },
    Rejected { version: u16, code: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_debug_is_redacted() {
        let token = LaunchToken::generate();
        assert!(!format!("{token:?}").contains(token.expose()));
        assert_eq!(LaunchToken::parse(token.expose()).unwrap(), token);
    }

    #[test]
    fn patch_supports_true_unset() {
        let snapshot = EnvironmentSnapshot {
            variables: BTreeMap::from([
                ("KEEP".into(), "yes".into()),
                ("REMOVE".into(), "secret".into()),
            ]),
        }
        .patched([("ADD".into(), "value".into())], ["REMOVE".to_string()]);
        assert_eq!(
            snapshot.variables.get("KEEP").map(String::as_str),
            Some("yes")
        );
        assert_eq!(
            snapshot.variables.get("ADD").map(String::as_str),
            Some("value")
        );
        assert!(!snapshot.variables.contains_key("REMOVE"));
    }

    #[test]
    fn launch_spec_requires_absolute_paths() {
        let spec = LaunchSpec {
            executable: PathBuf::from("claude"),
            args: Vec::new(),
            cwd: PathBuf::from("."),
            environment: EnvironmentSnapshot::default(),
        };
        assert_eq!(spec.validate(), Err("Claude executable must be absolute"));
    }

    #[test]
    fn launch_spec_debug_omits_arguments_and_environment_values() {
        let spec = LaunchSpec {
            executable: PathBuf::from("/bin/echo"),
            args: vec!["sensitive-system-prompt".into()],
            cwd: PathBuf::from("/tmp"),
            environment: EnvironmentSnapshot {
                variables: BTreeMap::from([("ANTHROPIC_API_KEY".into(), "secret-value".into())]),
            },
        };
        let debug = format!("{spec:?}");
        assert!(!debug.contains("sensitive-system-prompt"));
        assert!(!debug.contains("secret-value"));
        assert!(!debug.contains("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn launcher_request_debug_redacts_token() {
        let request = LauncherRequest {
            version: LAUNCHER_PROTOCOL_VERSION,
            token: LaunchToken::generate(),
        };
        assert!(!format!("{request:?}").contains(request.token.expose()));
    }
}
