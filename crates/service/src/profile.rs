use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Resolved profile configuration.
///
/// Claude Code-specific fields (`model`, `permission_mode`, `allowed_tools`, etc.)
/// are passed through to the caller. The Service layer does NOT automatically
/// convert these into CLI arguments — the daemon (`pmuxd`) or CLI (`pmux`) is
/// responsible for constructing the appropriate `--model`, `--permission-mode`,
/// etc. flags when starting a Claude Code session.
#[derive(Clone, Debug)]
pub struct ProfileSpec {
    pub agent: Option<String>,
    pub cwd: Option<PathBuf>,
    pub rows: Option<u16>,
    pub cols: Option<u16>,
    pub logging_mode: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    // Claude Code convenience fields
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub allowed_tools: Option<String>,
    pub disallowed_tools: Option<String>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub effort: Option<String>,
    pub max_budget: Option<f64>,
    pub extra_args: Vec<String>,
    pub settings: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
struct ProfileFile {
    #[serde(default)]
    profiles: HashMap<String, RawProfile>,
}

#[derive(Debug, Deserialize)]
struct RawProfile {
    #[serde(default)]
    agent: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    rows: Option<u16>,
    #[serde(default)]
    cols: Option<u16>,
    #[serde(default)]
    logging_mode: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    // Claude Code convenience fields
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    permission_mode: Option<String>,
    #[serde(default)]
    allowed_tools: Option<String>,
    #[serde(default)]
    disallowed_tools: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    append_system_prompt: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    max_budget: Option<f64>,
    #[serde(default)]
    extra_args: Vec<String>,
    #[serde(default)]
    settings: Option<toml::Value>,
}

pub fn load_profile(name: &str) -> Result<Option<ProfileSpec>> {
    for path in profile_candidates() {
        if !path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read profile file {}", path.display()))?;
        let parsed: ProfileFile = toml::from_str(&content)
            .with_context(|| format!("invalid profile TOML in {}", path.display()))?;
        if let Some(raw) = parsed.profiles.get(name) {
            tracing::debug!(path = %path.display(), profile = name, "loaded profile from file");
            return Ok(Some(ProfileSpec {
                agent: raw.agent.clone(),
                cwd: raw.cwd.as_ref().map(PathBuf::from),
                rows: raw.rows,
                cols: raw.cols,
                logging_mode: raw.logging_mode.clone(),
                args: raw.args.clone(),
                env: raw
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                model: raw.model.clone(),
                permission_mode: raw.permission_mode.clone(),
                allowed_tools: raw.allowed_tools.clone(),
                disallowed_tools: raw.disallowed_tools.clone(),
                system_prompt: raw.system_prompt.clone(),
                append_system_prompt: raw.append_system_prompt.clone(),
                effort: raw.effort.clone(),
                max_budget: raw.max_budget,
                extra_args: raw.extra_args.clone(),
                settings: raw.settings.clone(),
            }));
        }
    }
    Ok(None)
}

/// Convert a `toml::Value` to a `serde_json::Value` for settings pass-through.
pub fn toml_to_json(val: &toml::Value) -> serde_json::Value {
    match val {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(*i),
        toml::Value::Float(f) => serde_json::json!(*f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(t) => {
            let map: serde_json::Map<String, serde_json::Value> = t
                .iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect();
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
    }
}

pub fn profile_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(explicit) = std::env::var("PSEUDOMUX_PROFILE_FILE") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            out.push(PathBuf::from(trimmed));
        }
    }
    if let Some(repo) = repo_profile_file() {
        out.push(repo);
    }
    if let Some(user) = user_profile_file() {
        out.push(user);
    }
    out
}

fn repo_profile_file() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join(".pseudomux").join("profiles.toml"))
}

fn user_profile_file() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            return Some(Path::new(trimmed).join("pseudomux").join("profiles.toml"));
        }
    }
    dirs::home_dir().map(|home| home.join(".config").join("pseudomux").join("profiles.toml"))
}
