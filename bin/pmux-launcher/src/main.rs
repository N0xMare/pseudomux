use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use pseudomux_rmux::{
    LAUNCHER_PROTOCOL_VERSION, LaunchSpec, LaunchToken, LauncherRequest, LauncherResponse,
    MAX_LAUNCHER_FRAME_BYTES,
};

#[derive(Debug, Parser)]
#[command(
    name = "pmux-launcher",
    version,
    about = "Internal one-shot pmux launcher"
)]
struct Args {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long, hide = true)]
    token: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let token = LaunchToken::parse(args.token).map_err(anyhow::Error::msg)?;
    let spec = request_launch_spec(&args.socket, token)?;
    replace_process(spec)
}

#[cfg(unix)]
fn request_launch_spec(socket: &Path, token: LaunchToken) -> Result<LaunchSpec> {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::net::UnixStream;

    if !socket.is_absolute() {
        bail!("launcher socket must be absolute");
    }
    let metadata = std::fs::metadata(socket)
        .with_context(|| format!("launcher socket {} is unavailable", socket.display()))?;
    if !metadata.file_type().is_socket() {
        bail!("launcher endpoint is not a Unix socket");
    }

    let mut stream = UnixStream::connect(socket).context("failed to connect to launch broker")?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    write_frame(
        &mut stream,
        &LauncherRequest {
            version: LAUNCHER_PROTOCOL_VERSION,
            token,
        },
    )?;
    let response: LauncherResponse = read_frame(&mut stream)?;
    match response {
        LauncherResponse::Ready { version, spec } if version == LAUNCHER_PROTOCOL_VERSION => {
            spec.validate().map_err(anyhow::Error::msg)?;
            Ok(spec)
        }
        LauncherResponse::Ready { version, .. } => {
            bail!("launch broker returned unsupported version {version}")
        }
        LauncherResponse::Rejected { version, code } => {
            bail!("launch broker rejected request (version {version}, code {code})")
        }
    }
}

#[cfg(not(unix))]
fn request_launch_spec(_socket: &Path, _token: LaunchToken) -> Result<LaunchSpec> {
    bail!("pmux-launcher named-pipe transport is not implemented on this platform")
}

fn write_frame<T: serde::Serialize>(writer: &mut impl Write, value: &T) -> Result<()> {
    let payload = serde_json::to_vec(value)?;
    if payload.len() > MAX_LAUNCHER_FRAME_BYTES {
        bail!("launch request exceeds maximum frame size");
    }
    let length = u32::try_from(payload.len()).context("launch request is too large")?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

fn read_frame<T: serde::de::DeserializeOwned>(reader: &mut impl Read) -> Result<T> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_LAUNCHER_FRAME_BYTES {
        bail!("launch response exceeds maximum frame size");
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).context("invalid launch broker response")
}

#[cfg(unix)]
fn replace_process(spec: LaunchSpec) -> Result<()> {
    use std::os::unix::process::CommandExt;

    let error = Command::new(&spec.executable)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(&spec.environment.variables)
        .exec();
    Err(error).with_context(|| format!("failed to exec {}", spec.executable.display()))
}

#[cfg(not(unix))]
fn replace_process(spec: LaunchSpec) -> Result<()> {
    let status = Command::new(&spec.executable)
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(&spec.environment.variables)
        .status()
        .with_context(|| format!("failed to spawn {}", spec.executable.display()))?;
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_frame_is_rejected_before_allocation() {
        let length = (MAX_LAUNCHER_FRAME_BYTES as u32 + 1).to_be_bytes();
        let error = read_frame::<serde_json::Value>(&mut length.as_slice()).unwrap_err();
        assert!(error.to_string().contains("maximum frame size"));
    }

    #[test]
    fn request_frame_roundtrips() {
        let request = LauncherRequest {
            version: LAUNCHER_PROTOCOL_VERSION,
            token: LaunchToken::generate(),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &request).unwrap();
        let decoded: LauncherRequest = read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded.version, request.version);
        assert_eq!(decoded.token, request.token);
    }
}
