use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Immutable executable identities shared by shipped-binary process tests.
///
/// An explicit candidate directory is fail-closed: every named binary must be
/// a direct canonical regular executable in that one directory. Cargo paths
/// are considered only when the explicit directory is absent.
pub struct CandidateBinaries {
    files: BTreeMap<String, CandidateFile>,
}

struct CandidateFile {
    directory: PathBuf,
    directory_device: u64,
    directory_inode: u64,
    path: PathBuf,
    device: u64,
    inode: u64,
    length: u64,
    mode: u32,
    digest: [u8; 32],
}

impl CandidateBinaries {
    pub fn discover(
        exact_directory: Option<PathBuf>,
        cargo_fallbacks: impl IntoIterator<Item = (String, PathBuf)>,
    ) -> Result<Self, String> {
        let fallbacks = cargo_fallbacks.into_iter().collect::<BTreeMap<_, _>>();
        if fallbacks.is_empty() {
            return Err("candidate binary set must not be empty".to_owned());
        }

        let exact_directory = exact_directory.map(validate_exact_directory).transpose()?;
        let mut files = BTreeMap::new();
        for (name, fallback) in fallbacks {
            let unresolved = exact_directory
                .as_ref()
                .map_or(fallback, |directory| directory.join(&name));
            let file = CandidateFile::capture(&name, unresolved, exact_directory.as_deref())?;
            files.insert(name, file);
        }
        let candidates = Self { files };
        candidates.assert_unchanged()?;
        Ok(candidates)
    }

    pub fn path(&self, name: &str) -> &Path {
        let file = self
            .files
            .get(name)
            .unwrap_or_else(|| panic!("candidate {name} was not registered"));
        file.assert_unchanged(name)
            .unwrap_or_else(|error| panic!("candidate changed before launch: {error}"));
        &file.path
    }

    pub fn assert_unchanged(&self) -> Result<(), String> {
        for (name, file) in &self.files {
            file.assert_unchanged(name)?;
        }
        Ok(())
    }
}

impl CandidateFile {
    fn capture(
        name: &str,
        unresolved: PathBuf,
        exact_directory: Option<&Path>,
    ) -> Result<Self, String> {
        if !unresolved.is_absolute() {
            return Err(format!(
                "candidate {name} path must be absolute: {}",
                unresolved.display()
            ));
        }
        let link_metadata = fs::symlink_metadata(&unresolved).map_err(|error| {
            format!(
                "required candidate {name} is unavailable ({}): {error}",
                unresolved.display()
            )
        })?;
        if !link_metadata.file_type().is_file() {
            return Err(format!(
                "candidate {name} must be a direct regular file: {}",
                unresolved.display()
            ));
        }
        let path = fs::canonicalize(&unresolved).map_err(|error| {
            format!(
                "candidate {name} cannot be canonicalized ({}): {error}",
                unresolved.display()
            )
        })?;
        if path != unresolved {
            return Err(format!(
                "candidate {name} must not be a path alias: {} != {}",
                unresolved.display(),
                path.display()
            ));
        }
        let directory = path
            .parent()
            .ok_or_else(|| format!("candidate {name} has no parent directory"))?
            .to_path_buf();
        if exact_directory.is_some_and(|exact| exact != directory) {
            return Err(format!(
                "candidate {name} escaped the exact directory: {}",
                path.display()
            ));
        }
        let directory_metadata = fs::metadata(&directory).map_err(|error| {
            format!(
                "failed to inspect candidate {name} directory {}: {error}",
                directory.display()
            )
        })?;
        if !directory_metadata.is_dir() {
            return Err(format!(
                "candidate {name} parent is not a directory: {}",
                directory.display()
            ));
        }
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("failed to inspect candidate {name}: {error}"))?;
        let mode = metadata.permissions().mode();
        if mode & 0o111 == 0 {
            return Err(format!("candidate {name} is not executable"));
        }
        let first_digest = digest_file(&path)?;
        let second_digest = digest_file(&path)?;
        let after = fs::metadata(&path)
            .map_err(|error| format!("failed to re-inspect candidate {name}: {error}"))?;
        if first_digest != second_digest
            || after.dev() != metadata.dev()
            || after.ino() != metadata.ino()
            || after.len() != metadata.len()
            || after.permissions().mode() != mode
        {
            return Err(format!(
                "candidate {name} changed while its initial identity was captured"
            ));
        }

        Ok(Self {
            directory,
            directory_device: directory_metadata.dev(),
            directory_inode: directory_metadata.ino(),
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            mode,
            digest: first_digest,
        })
    }

    fn assert_unchanged(&self, name: &str) -> Result<(), String> {
        let directory_metadata = fs::metadata(&self.directory).map_err(|error| {
            format!(
                "candidate {name} directory disappeared ({}): {error}",
                self.directory.display()
            )
        })?;
        if !directory_metadata.is_dir()
            || directory_metadata.dev() != self.directory_device
            || directory_metadata.ino() != self.directory_inode
        {
            return Err(format!(
                "candidate {name} directory changed filesystem identity"
            ));
        }
        let canonical = fs::canonicalize(&self.path)
            .map_err(|error| format!("candidate {name} disappeared: {error}"))?;
        let link_metadata = fs::symlink_metadata(&self.path)
            .map_err(|error| format!("candidate {name} disappeared: {error}"))?;
        let before = fs::metadata(&self.path)
            .map_err(|error| format!("candidate {name} disappeared: {error}"))?;
        if canonical != self.path
            || canonical.parent() != Some(self.directory.as_path())
            || !link_metadata.file_type().is_file()
            || before.dev() != self.device
            || before.ino() != self.inode
            || before.len() != self.length
            || before.permissions().mode() != self.mode
            || self.mode & 0o111 == 0
        {
            return Err(format!(
                "candidate {name} changed regular executable identity"
            ));
        }
        let digest = digest_file(&self.path)?;
        let after = fs::metadata(&self.path)
            .map_err(|error| format!("candidate {name} disappeared after hashing: {error}"))?;
        if digest != self.digest
            || after.dev() != self.device
            || after.ino() != self.inode
            || after.len() != self.length
            || after.permissions().mode() != self.mode
        {
            return Err(format!(
                "candidate {name} changed content or filesystem identity"
            ));
        }
        Ok(())
    }
}

fn validate_exact_directory(directory: PathBuf) -> Result<PathBuf, String> {
    if !directory.is_absolute() {
        return Err("PMUX_TEST_BIN_DIR must be absolute".to_owned());
    }
    let link_metadata = fs::symlink_metadata(&directory).map_err(|error| {
        format!(
            "could not inspect PMUX_TEST_BIN_DIR {}: {error}",
            directory.display()
        )
    })?;
    if !link_metadata.file_type().is_dir() {
        return Err("PMUX_TEST_BIN_DIR must name one real directory, not an alias".to_owned());
    }
    let canonical = fs::canonicalize(&directory).map_err(|error| {
        format!(
            "could not canonicalize PMUX_TEST_BIN_DIR {}: {error}",
            directory.display()
        )
    })?;
    if canonical != directory {
        return Err(format!(
            "PMUX_TEST_BIN_DIR must be canonical: {} != {}",
            directory.display(),
            canonical.display()
        ));
    }
    Ok(directory)
}

fn digest_file(path: &Path) -> Result<[u8; 32], String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("failed to open candidate {}: {error}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("failed to hash candidate {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().into())
}
