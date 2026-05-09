mod handler;
mod http;
mod session;
mod util;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use handler::handle_client;
use pseudomux_service::{Service, socket_candidates};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::net::{UnixListener, UnixStream};
use tokio::time::{Duration, timeout};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Parser, Debug)]
#[command(name = "pmuxd")]
#[command(about = "pseudomux daemon", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Serve {
        #[arg(long)]
        socket: Option<PathBuf>,
        #[arg(long)]
        http_port: Option<u16>,
        #[arg(long, default_value = "127.0.0.1")]
        http_host: String,
        #[arg(long, env = "PSEUDOMUX_HTTP_TOKEN")]
        http_token: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    #[allow(unsafe_code)]
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }

    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            socket,
            http_port,
            http_host,
            http_token,
        } => run_server(socket, http_port, http_host, http_token).await?,
    }
    Ok(())
}

async fn run_server(
    socket_override: Option<PathBuf>,
    http_port: Option<u16>,
    http_host: String,
    http_token: Option<String>,
) -> Result<()> {
    let service = Arc::new(Service::new().context("failed to init service")?);
    let log_dir = daemon_log_dir(service.log_root());
    let _log_guard = init_logging(&log_dir)?;
    let socket_path = resolve_socket_path(socket_override)?;
    if socket_path.exists() {
        if socket_listener_alive(&socket_path).await {
            bail!(
                "socket {} already has a live listener",
                socket_path.display()
            );
        }
        remove_stale_socket(&socket_path).await?;
    }
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind socket {}", socket_path.display()))?;
    set_socket_permissions(&socket_path)?;
    info!(socket = %socket_path.display(), "pmuxd listening");
    if let Some(port) = http_port {
        let svc = Arc::clone(&service);
        tokio::spawn(async move {
            if let Err(e) = http::run_http_server(svc, http_host, port, http_token).await {
                error!(error = ?e, "HTTP server error");
            }
        });
    }
    loop {
        let (stream, _) = listener.accept().await?;
        let svc = Arc::clone(&service);
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, svc).await {
                error!(error = ?err, "client error");
            }
        });
    }
}

async fn socket_listener_alive(path: &Path) -> bool {
    matches!(
        timeout(Duration::from_millis(250), UnixStream::connect(path)).await,
        Ok(Ok(_))
    )
}

async fn remove_stale_socket(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        let meta = std::fs::metadata(path)
            .with_context(|| format!("failed to inspect stale socket {}", path.display()))?;
        if !meta.file_type().is_socket() {
            bail!("refusing to remove non-socket file at {}", path.display());
        }
        #[allow(unsafe_code)]
        let uid = unsafe { libc::geteuid() };
        if meta.uid() != uid {
            bail!(
                "refusing to remove socket {} owned by uid {}",
                path.display(),
                meta.uid()
            );
        }
    }
    fs::remove_file(path)
        .await
        .with_context(|| format!("failed to remove stale socket {}", path.display()))?;
    Ok(())
}

fn set_socket_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set socket permissions {}", path.display()))?;
    }
    Ok(())
}

fn daemon_log_dir(sessions_root: &Path) -> PathBuf {
    sessions_root
        .parent()
        .map_or_else(|| sessions_root.to_path_buf(), Path::to_path_buf)
        .join("logs")
}

fn init_logging(log_dir: &Path) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    std::fs::create_dir_all(log_dir)
        .with_context(|| format!("failed to create log dir {}", log_dir.display()))?;
    let file_appender = tracing_appender::rolling::never(log_dir, "pmuxd.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let startup_log = log_dir.join("pmuxd.log");
    if let Ok(mut file) = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&startup_log)
    {
        let _ = writeln!(
            file,
            "{{\"event\":\"startup\",\"pid\":{}}}",
            std::process::id()
        );
    }

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_ansi(false)
                .with_writer(file_writer),
        )
        .try_init()
        .ok();
    Ok(guard)
}

fn resolve_socket_path(socket_override: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(explicit) = socket_override {
        ensure_parent(&explicit)?;
        return Ok(explicit);
    }
    let mut errors = Vec::new();
    for candidate in socket_candidates() {
        if let Err(err) = ensure_parent(&candidate) {
            errors.push(format!("{}: {}", candidate.display(), err));
            continue;
        }
        return Ok(candidate);
    }
    if errors.is_empty() {
        bail!("no socket path candidates available");
    }
    bail!(
        "unable to prepare pmuxd socket directory: {}",
        errors.join("; ")
    )
}

fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let meta = std::fs::metadata(parent)?;
            #[allow(unsafe_code)]
            let uid = unsafe { libc::geteuid() };
            if meta.uid() == uid {
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
            }
        }
    }
    Ok(())
}
