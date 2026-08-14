use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const CAPACITY_RECORD: &[u8] = b"{\"event\":\"log_capacity_reached\"}\n";

/// Append-only writer with one exact retained backup and a hard byte ceiling.
///
/// The tracing worker gives this writer one complete formatted event per
/// `write_all` call. An event that would consume the reserved capacity record
/// is dropped atomically, the record is written once, and later events are
/// discarded until the next daemon start rotates the capped file.
#[derive(Debug)]
pub(crate) struct BoundedLogWriter {
    file: File,
    written: u64,
    maximum_bytes: u64,
    full_marker: PathBuf,
    capped: bool,
}

impl BoundedLogWriter {
    pub(crate) fn open(directory: &Path, maximum_bytes: u64) -> io::Result<Self> {
        if maximum_bytes <= CAPACITY_RECORD.len() as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pmuxd log capacity cannot fit its terminal record",
            ));
        }
        let current = directory.join("pmuxd.log");
        let backup = directory.join("pmuxd.log.previous");
        let full_marker = directory.join(".pmuxd.log.full");
        validate_log_entry(&current)?;
        validate_log_entry(&backup)?;
        validate_log_entry(&full_marker)?;

        let current_bytes = current.metadata().map_or(0, |metadata| metadata.len());
        if full_marker.exists() || current_bytes >= maximum_bytes {
            remove_regular_if_present(&backup)?;
            if current.exists() {
                std::fs::rename(&current, &backup)?;
                let backup_file = OpenOptions::new().write(true).open(&backup)?;
                if backup_file.metadata()?.len() > maximum_bytes {
                    backup_file.set_len(maximum_bytes)?;
                }
                set_owner_only(&backup_file)?;
            }
            remove_regular_if_present(&full_marker)?;
        }

        let file = open_owner_only_append(&current)?;
        let written = file.metadata()?.len();
        Ok(Self {
            file,
            written,
            maximum_bytes,
            full_marker,
            capped: false,
        })
    }

    fn mark_capped(&mut self) -> io::Result<()> {
        if self.capped {
            return Ok(());
        }
        self.file.write_all(CAPACITY_RECORD)?;
        self.written += CAPACITY_RECORD.len() as u64;
        self.file.flush()?;
        let mut marker = open_owner_only_new(&self.full_marker)?;
        marker.write_all(b"full\n")?;
        marker.flush()?;
        self.capped = true;
        Ok(())
    }
}

impl Write for BoundedLogWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.capped {
            return Ok(bytes.len());
        }
        let payload_limit = self.maximum_bytes - CAPACITY_RECORD.len() as u64;
        let incoming = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if self
            .written
            .checked_add(incoming)
            .is_some_and(|total| total <= payload_limit)
        {
            self.file.write_all(bytes)?;
            self.written += incoming;
        } else {
            self.mark_capped()?;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn open_owner_only_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    configure_owner_only(&mut options);
    let file = options.open(path)?;
    set_owner_only(&file)?;
    Ok(file)
}

fn open_owner_only_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    configure_owner_only(&mut options);
    let file = options.open(path)?;
    set_owner_only(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn configure_owner_only(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn configure_owner_only(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn set_owner_only(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only(_file: &File) -> io::Result<()> {
    Ok(())
}

fn validate_log_entry(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("pmuxd log entry is not a regular file: {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_regular_if_present(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => std::fs::remove_file(path),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("refusing to replace non-file log entry: {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn runtime_growth_is_capped_and_next_open_rotates_exactly_one_backup() {
        let directory = tempfile::tempdir().unwrap();
        let maximum = 256;
        let mut writer = BoundedLogWriter::open(directory.path(), maximum).unwrap();
        writer.write_all(&[b'a'; 180]).unwrap();
        writer.write_all(&[b'b'; 100]).unwrap();
        writer.write_all(&[b'c'; 100]).unwrap();
        writer.flush().unwrap();
        drop(writer);

        let current = directory.path().join("pmuxd.log");
        let contents = std::fs::read(&current).unwrap();
        assert!(contents.len() <= maximum as usize);
        assert_eq!(
            contents
                .windows(CAPACITY_RECORD.len())
                .filter(|window| *window == CAPACITY_RECORD)
                .count(),
            1
        );
        assert_eq!(
            std::fs::metadata(&current).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let mut replacement = BoundedLogWriter::open(directory.path(), maximum).unwrap();
        replacement.write_all(b"next-start\n").unwrap();
        replacement.flush().unwrap();
        drop(replacement);
        assert_eq!(std::fs::read(&current).unwrap(), b"next-start\n");
        let previous = directory.path().join("pmuxd.log.previous");
        assert!(std::fs::metadata(&previous).unwrap().len() <= maximum);
        assert!(!directory.path().join(".pmuxd.log.full").exists());
    }

    #[test]
    fn oversized_preexisting_log_is_bounded_during_rotation() {
        let directory = tempfile::tempdir().unwrap();
        let current = directory.path().join("pmuxd.log");
        std::fs::write(&current, vec![b'x'; 1024]).unwrap();

        let writer = BoundedLogWriter::open(directory.path(), 128).unwrap();
        drop(writer);
        assert_eq!(
            std::fs::metadata(directory.path().join("pmuxd.log.previous"))
                .unwrap()
                .len(),
            128
        );
        assert_eq!(std::fs::metadata(current).unwrap().len(), 0);
    }

    #[test]
    fn non_file_entries_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("pmuxd.log")).unwrap();
        let error = match BoundedLogWriter::open(directory.path(), 256) {
            Ok(_) => panic!("a directory at the log path must fail closed"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
