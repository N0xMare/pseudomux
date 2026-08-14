#![allow(
    dead_code,
    reason = "shared integration-test support is compiled independently for each test target"
)]
#![allow(
    unsafe_code,
    reason = "tests signal only exact, start-time-fenced child identities and query POSIX process metadata"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};

pub mod actual_daemon;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub start_token: String,
    pub process_group_id: i32,
    pub session_id: i32,
    marker: String,
}

impl ProcessIdentity {
    pub fn capture(pid: u32, marker: impl Into<String>) -> Result<Self> {
        ensure!(pid > 0, "process identity requires a positive pid");
        let marker = marker.into();
        ensure!(
            !marker.is_empty(),
            "process identity marker must not be empty"
        );
        let before = process_start_token(pid)?
            .with_context(|| format!("process {pid} disappeared before identity capture"))?;
        let status = ps_field(pid, "stat")?
            .with_context(|| format!("process {pid} disappeared during identity capture"))?;
        ensure!(
            !status.trim_start().starts_with('Z'),
            "process {pid} was already a zombie during identity capture"
        );
        let command = ps_field(pid, "command")?.with_context(|| {
            format!("process {pid} command disappeared during identity capture")
        })?;
        ensure!(
            command.contains(&marker),
            "process {pid} command did not contain exact marker {marker:?}: {command:?}"
        );
        let process_group_id = get_process_group(pid)?
            .with_context(|| format!("process {pid} disappeared before pgid capture"))?;
        let session_id = get_session_id(pid)?
            .with_context(|| format!("process {pid} disappeared before sid capture"))?;
        let after = process_start_token(pid)?
            .with_context(|| format!("process {pid} disappeared after identity capture"))?;
        ensure!(
            before == after,
            "process {pid} changed start identity during capture"
        );
        Ok(Self {
            pid,
            start_token: before,
            process_group_id,
            session_id,
            marker,
        })
    }

    pub fn assert_running(&self) -> Result<()> {
        let start = process_start_token(self.pid)?
            .with_context(|| format!("exact process {} is absent", self.pid))?;
        ensure!(
            start == self.start_token,
            "pid {} was reused (expected start {}, observed {})",
            self.pid,
            self.start_token,
            start
        );
        let status = ps_field(self.pid, "stat")?
            .with_context(|| format!("exact process {} status is absent", self.pid))?;
        ensure!(
            !status.trim_start().starts_with('Z'),
            "exact process {} is a zombie",
            self.pid
        );
        let command = ps_field(self.pid, "command")?
            .with_context(|| format!("exact process {} command is absent", self.pid))?;
        ensure!(
            command.contains(&self.marker),
            "exact process {} changed command identity: {command:?}",
            self.pid
        );
        ensure!(
            get_process_group(self.pid)? == Some(self.process_group_id),
            "exact process {} changed process group",
            self.pid
        );
        ensure!(
            get_session_id(self.pid)? == Some(self.session_id),
            "exact process {} changed POSIX session",
            self.pid
        );
        Ok(())
    }

    pub fn is_present(&self) -> Result<bool> {
        Ok(process_start_token(self.pid)?.as_deref() == Some(self.start_token.as_str()))
    }

    pub fn signal(&self, signal: libc::c_int) -> Result<()> {
        self.assert_running()?;
        let pid = i32::try_from(self.pid).context("exact pid does not fit pid_t")?;
        if unsafe { libc::kill(pid, signal) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
                .with_context(|| format!("failed to signal exact process {}", self.pid))
        }
    }
}

pub struct ExactProcessGuard {
    identity: Option<ProcessIdentity>,
}

impl ExactProcessGuard {
    pub fn new(identity: ProcessIdentity) -> Self {
        Self {
            identity: Some(identity),
        }
    }

    pub fn identity(&self) -> &ProcessIdentity {
        self.identity.as_ref().expect("process guard remains armed")
    }

    pub fn disarm(&mut self) {
        self.identity = None;
    }
}

impl Drop for ExactProcessGuard {
    fn drop(&mut self) {
        let Some(identity) = self.identity.as_ref() else {
            return;
        };
        if identity.assert_running().is_err() {
            return;
        }
        let Ok(pid) = i32::try_from(identity.pid) else {
            return;
        };
        if identity.session_id == pid && identity.process_group_id == pid {
            // The exact still-live session leader fences this negative process
            // group signal. It cannot target an unrelated reused group.
            let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

pub async fn wait_for_process_absence(identity: &ProcessIdentity, timeout: Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if !identity.is_present()? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "exact process survived cleanup: pid={} start={} pgid={} sid={}",
                identity.pid,
                identity.start_token,
                identity.process_group_id,
                identity.session_id
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub async fn wait_for_pid_file(path: &Path, timeout: Duration) -> Result<u32> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(contents) = std::fs::read_to_string(path)
            && let Ok(pid) = contents.trim().parse::<u32>()
            && pid > 0
        {
            return Ok(pid);
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("timed out waiting for exact pid file {}", path.display());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub fn find_direct_child(parent_pid: u32, markers: &[&str]) -> Result<u32> {
    ensure!(!markers.is_empty(), "at least one child marker is required");
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
        .context("failed to inspect exact child process tree")?;
    ensure!(
        output.status.success(),
        "/bin/ps failed while locating child"
    );
    let mut matches = Vec::new();
    for line in String::from_utf8(output.stdout)
        .context("/bin/ps emitted non-UTF-8 process data")?
        .lines()
    {
        let mut fields = line.split_whitespace();
        let Some(pid) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let Some(ppid) = fields.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let command = fields.collect::<Vec<_>>().join(" ");
        if ppid == parent_pid && markers.iter().all(|marker| command.contains(marker)) {
            matches.push(pid);
        }
    }
    ensure!(
        matches.len() == 1,
        "expected one exact direct child of {parent_pid} matching {markers:?}, found {matches:?}"
    );
    Ok(matches[0])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SocketIdentity {
    pub device: u64,
    pub inode: u64,
}

impl SocketIdentity {
    pub fn capture(path: &Path) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect socket {}", path.display()))?;
        ensure!(
            metadata.file_type().is_socket(),
            "owned endpoint is not a socket: {}",
            path.display()
        );
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    pub fn remains_at(self, path: &Path) -> Result<bool> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => Ok(metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error)
                .with_context(|| format!("failed to re-inspect socket {}", path.display())),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ProcessResources {
    pub rss_kib: u64,
    pub open_fds: usize,
}

pub fn process_resources(identity: &ProcessIdentity) -> Result<ProcessResources> {
    identity.assert_running()?;
    let rss_kib = ps_field(identity.pid, "rss")?
        .context("exact process RSS disappeared")?
        .trim()
        .parse::<u64>()
        .context("exact process RSS was not numeric")?;
    let open_fds = exact_open_fd_count(identity.pid)?;
    identity.assert_running()?;
    Ok(ProcessResources { rss_kib, open_fds })
}

#[cfg(target_os = "linux")]
pub fn exact_open_fd_count(pid: u32) -> Result<usize> {
    Ok(std::fs::read_dir(format!("/proc/{pid}/fd"))
        .with_context(|| format!("failed to inspect descriptors for exact process {pid}"))?
        .count())
}

#[cfg(target_os = "macos")]
pub fn exact_open_fd_count(pid: u32) -> Result<usize> {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid.to_string(), "-Fn"])
        .output()
        .with_context(|| format!("failed to run lsof for exact process {pid}"))?;
    ensure!(
        output.status.success(),
        "lsof failed for exact process {pid}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)
        .context("lsof emitted non-UTF-8 descriptor data")?
        .lines()
        .filter(|line| line.starts_with('f'))
        .count())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn exact_open_fd_count(_pid: u32) -> Result<usize> {
    bail!("exact descriptor observation is unsupported on this Unix platform")
}

pub fn runtime_entries(root: &Path) -> Result<BTreeSet<PathBuf>> {
    fn visit(root: &Path, current: &Path, entries: &mut BTreeSet<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(current)
            .with_context(|| format!("failed to read runtime directory {}", current.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            entries.insert(path.strip_prefix(root)?.to_path_buf());
            if entry.file_type()?.is_dir() {
                visit(root, &path, entries)?;
            }
        }
        Ok(())
    }

    let mut entries = BTreeSet::new();
    visit(root, root, &mut entries)?;
    Ok(entries)
}

pub fn set_owner_only(path: &Path) -> Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to secure {}", path.display()))
}

pub struct CandidateFiles {
    directory: PathBuf,
    directory_device: u64,
    directory_inode: u64,
    files: BTreeMap<String, CandidateFile>,
}

struct CandidateFile {
    path: PathBuf,
    device: u64,
    inode: u64,
    digest: [u8; 32],
}

impl CandidateFiles {
    pub fn discover(names: &[&str]) -> Result<Self> {
        let directory = if let Some(directory) =
            std::env::var_os("PMUX_TEST_BIN_DIR").or_else(|| std::env::var_os("PMUX_E2E_BIN_DIR"))
        {
            PathBuf::from(directory)
        } else {
            std::env::current_exe()
                .context("failed to locate integration-test executable")?
                .parent()
                .and_then(Path::parent)
                .context("integration-test executable has no candidate directory")?
                .to_path_buf()
        }
        .canonicalize()
        .context("failed to canonicalize candidate binary directory")?;
        ensure!(
            directory.is_absolute(),
            "candidate directory must be absolute"
        );
        let directory_metadata = std::fs::metadata(&directory)
            .context("failed to inspect candidate binary directory")?;
        ensure!(
            directory_metadata.is_dir(),
            "candidate binary root is not a directory"
        );

        let mut files = BTreeMap::new();
        for name in names {
            let path = directory
                .join(name)
                .canonicalize()
                .with_context(|| format!("required candidate {name} is unavailable"))?;
            ensure!(
                path.parent() == Some(directory.as_path()),
                "candidate escaped exact binary directory: {}",
                path.display()
            );
            let metadata = std::fs::metadata(&path)?;
            ensure!(
                metadata.is_file(),
                "candidate is not a file: {}",
                path.display()
            );
            ensure!(
                metadata.permissions().mode() & 0o111 != 0,
                "candidate is not executable: {}",
                path.display()
            );
            files.insert(
                (*name).to_owned(),
                CandidateFile {
                    path: path.clone(),
                    device: metadata.dev(),
                    inode: metadata.ino(),
                    digest: digest_file(&path)?,
                },
            );
        }
        Ok(Self {
            directory,
            directory_device: directory_metadata.dev(),
            directory_inode: directory_metadata.ino(),
            files,
        })
    }

    pub fn path(&self, name: &str) -> &Path {
        &self
            .files
            .get(name)
            .unwrap_or_else(|| panic!("candidate {name} was not registered"))
            .path
    }

    pub fn assert_unchanged(&self) -> Result<()> {
        let directory_metadata = std::fs::metadata(&self.directory)
            .context("candidate binary directory disappeared during lifecycle evidence")?;
        ensure!(
            directory_metadata.is_dir()
                && directory_metadata.dev() == self.directory_device
                && directory_metadata.ino() == self.directory_inode,
            "candidate binary directory changed filesystem identity during lifecycle evidence"
        );
        for (name, candidate) in &self.files {
            let path = &candidate.path;
            ensure!(
                path.parent() == Some(self.directory.as_path()),
                "candidate {name} changed directory identity"
            );
            ensure!(
                path.canonicalize()? == *path,
                "candidate {name} path changed canonical identity"
            );
            let metadata = std::fs::metadata(path).with_context(|| {
                format!("candidate {name} disappeared during lifecycle evidence")
            })?;
            ensure!(
                metadata.is_file()
                    && metadata.dev() == candidate.device
                    && metadata.ino() == candidate.inode,
                "candidate {name} changed filesystem identity during lifecycle evidence"
            );
            ensure!(
                digest_file(path)? == candidate.digest,
                "candidate {name} changed during lifecycle evidence"
            );
        }
        Ok(())
    }
}

fn digest_file(path: &Path) -> Result<[u8; 32]> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read candidate {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(hasher.finalize().into())
}

#[cfg(target_os = "linux")]
fn process_start_token(pid: u32) -> Result<Option<String>> {
    let path = format!("/proc/{pid}/stat");
    let stat = match std::fs::read_to_string(&path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("failed to read {path}")),
    };
    let close = stat
        .rfind(')')
        .context("Linux process stat did not contain a command terminator")?;
    let fields = stat[close + 1..].split_whitespace().collect::<Vec<_>>();
    let start_ticks = fields
        .get(19)
        .context("Linux process stat did not contain start-time ticks")?;
    Ok(Some(format!("linux-start-ticks:{start_ticks}")))
}

#[cfg(target_os = "macos")]
fn process_start_token(pid: u32) -> Result<Option<String>> {
    let pid = i32::try_from(pid).context("pid does not fit macOS proc_pidinfo")?;
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let size_i32 = i32::try_from(size).context("proc_bsdinfo size does not fit c_int")?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let read = unsafe {
        // SAFETY: info is writable memory exactly sized for PROC_PIDTBSDINFO.
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size_i32,
        )
    };
    if usize::try_from(read).ok() != Some(size) {
        let error = io::Error::last_os_error();
        return if matches!(error.raw_os_error(), Some(libc::ESRCH | libc::ENOENT)) {
            Ok(None)
        } else {
            Err(error)
                .with_context(|| format!("proc_pidinfo({pid}) did not return a full BSD identity"))
        };
    }
    let info = unsafe {
        // SAFETY: proc_pidinfo reported initialization of the complete structure.
        info.assume_init()
    };
    if info.pbi_pid != pid as u32 {
        return Ok(None);
    }
    Ok(Some(format!(
        "macos-start-time:{}:{}",
        info.pbi_start_tvsec, info.pbi_start_tvusec
    )))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_token(pid: u32) -> Result<Option<String>> {
    Ok(ps_field(pid, "lstart")?.map(|value| format!("ps-lstart:{}", value.trim())))
}

fn ps_field(pid: u32, field: &str) -> Result<Option<String>> {
    let output = Command::new("/bin/ps")
        .arg("-p")
        .arg(pid.to_string())
        .args(["-o", &format!("{field}=")])
        .output()
        .with_context(|| format!("failed to inspect process {pid} field {field}"))?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = String::from_utf8(output.stdout)
        .with_context(|| format!("process {pid} field {field} was not UTF-8"))?;
    let value = value.trim().to_owned();
    Ok((!value.is_empty()).then_some(value))
}

fn get_process_group(pid: u32) -> Result<Option<i32>> {
    let pid = i32::try_from(pid).context("pid does not fit pid_t")?;
    let result = unsafe { libc::getpgid(pid) };
    if result >= 0 {
        return Ok(Some(result));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(error).context("getpgid failed")
    }
}

fn get_session_id(pid: u32) -> Result<Option<i32>> {
    let pid = i32::try_from(pid).context("pid does not fit pid_t")?;
    let result = unsafe { libc::getsid(pid) };
    if result >= 0 {
        return Ok(Some(result));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(error).context("getsid failed")
    }
}
