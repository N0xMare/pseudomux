use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn sessions_root() -> anyhow::Result<PathBuf> {
    if let Some(dir) = env_override_sessions()? {
        return Ok(dir);
    }
    let base = default_state_dir()?;
    ensure_dir(base.join("sessions"))
}

pub fn socket_candidates() -> Vec<PathBuf> {
    let mut seen = HashSet::<PathBuf>::new();
    let mut out = Vec::new();

    if let Some(explicit) = explicit_socket_path() {
        push_unique(explicit, &mut out, &mut seen);
    }

    if let Some(env_dir) = env_state_dir() {
        push_unique(env_dir.join("pmux.sock"), &mut out, &mut seen);
    }

    if let Ok(default_dir) = default_state_dir() {
        push_unique(default_dir.join("pmux.sock"), &mut out, &mut seen);
    }

    if let Some(repo_dir) = repo_state_dir() {
        push_unique(repo_dir.join("pmux.sock"), &mut out, &mut seen);
    }

    push_unique(PathBuf::from("/tmp/pmux.sock"), &mut out, &mut seen);

    out
}

fn push_unique(path: PathBuf, out: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>) {
    if seen.insert(path.clone()) {
        out.push(path);
    }
}

fn env_override_sessions() -> anyhow::Result<Option<PathBuf>> {
    if let Some(dir) = env_state_dir() {
        return ensure_dir(dir.join("sessions")).map(Some);
    }
    Ok(None)
}

fn default_state_dir() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("home dir not found"))?;
    let path = if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("pseudomux")
    } else if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        PathBuf::from(xdg).join("pseudomux")
    } else {
        home.join(".local").join("state").join("pseudomux")
    };
    ensure_dir(path)
}

fn ensure_dir<P: AsRef<Path>>(path: P) -> anyhow::Result<PathBuf> {
    let path = path.as_ref();
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path.to_path_buf())
}

fn explicit_socket_path() -> Option<PathBuf> {
    std::env::var("PSEUDOMUX_SOCKET")
        .ok()
        .map(|val| val.trim().to_string())
        .filter(|val| !val.is_empty())
        .map(PathBuf::from)
}

fn env_state_dir() -> Option<PathBuf> {
    std::env::var("PSEUDOMUX_STATE_DIR")
        .ok()
        .map(|val| val.trim().to_string())
        .filter(|val| !val.is_empty())
        .map(PathBuf::from)
}

fn repo_state_dir() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .map(|cwd| cwd.join(".pseudomux"))
        .filter(|p| !p.as_os_str().is_empty())
}
