//! Creating a directory that is owner-only at every level pmux made.
//!
//! # Why this module exists
//!
//! `std::fs::create_dir_all(path)` followed by one `chmod(path, 0o700)` is the
//! shape this codebase wrote twice, and it is wrong the same way both times:
//! `create_dir_all` creates every MISSING ANCESTOR too, each at
//! `0o777 & !umask`, and the single `chmod` seals only the leaf. The sentence
//! the code means is "pmux made this tree private"; the sentence it enforces is
//! "pmux made the last component private".
//!
//! Both instances were MEASURED on this host before this module existed, with
//! the process umask at `022`:
//!
//! ```text
//! # pmuxd serve --socket /tmp/pmux-14th/deep/run/pmux.sock
//! drwxr-xr-x  /tmp/pmux-14th
//! drwxr-xr-x  /tmp/pmux-14th/deep
//! drwx------  /tmp/pmux-14th/deep/run     <- the only level the chmod reached
//!
//! # historical: pmuxd serve --path-b-parent /tmp/pmux-parent-probe ...
//! drwxr-xr-x  /tmp/pmux-parent-probe
//! drwxr-xr-x  /tmp/pmux-parent-probe/0
//! drwx------  /tmp/pmux-parent-probe/0/0  <- likewise
//! ```
//!
//! The pool case is the one with a durable payload: the 0755 levels survive
//! daemon shutdown, and their entries enumerate pool size, epoch counters
//! (= recycle count) and per-turn timing to any local user.
//!
//! # Why the mode is passed to `mkdir(2)` and not only chmod'd afterwards
//!
//! `create_dir_all` + `chmod` also leaves a window between the two calls in
//! which the directory is world-readable and a local user can open a handle to
//! it that survives the chmod. [`std::os::unix::fs::DirBuilderExt::mode`]
//! passes the mode to
//! `mkdir(2)`, so the directory is never observable at a wider mode. `umask`
//! can only CLEAR bits, and `0o700` has no group or other bits to clear, so the
//! created mode is `0o700` under every umask. The explicit `set_permissions`
//! that follows is for the level this call did not create.

use std::io;
use std::path::{Path, PathBuf};

/// The one mode every directory pmux creates for itself is born with and kept
/// at.
pub const OWNER_ONLY: u32 = 0o700;

/// Create `path` and every missing ancestor, each owner-only from birth.
///
/// Returns the directories this call created, OUTERMOST FIRST, so a caller --
/// or a test -- can name exactly what came into existence rather than infer it.
/// A directory that already existed is not touched and is not returned: this
/// function creates privately, it does not re-permission an operator's tree.
/// Callers that must also assert something about an existing leaf do that
/// themselves; see `pmuxd`'s `ensure_private_directory` and the pool's
/// `create_owner_only`.
///
/// # Errors
///
/// Any `stat` or `mkdir` failure, verbatim. A level another process created
/// between this call's `stat` and its `mkdir` is not an error and is not
/// reported as created, because it was not.
pub fn create_private_dir_all(path: &Path) -> io::Result<Vec<PathBuf>> {
    // Walk UP to the first ancestor that exists, then create DOWN. This is what
    // makes the intermediate levels visible to the loop at all: they are
    // exactly the entries between the deepest existing ancestor and the leaf,
    // and they are what `create_dir_all` creates without telling anyone.
    let mut missing: Vec<&Path> = Vec::new();
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => missing.push(ancestor),
            Err(error) => return Err(error),
        }
    }
    missing.reverse();

    let mut created = Vec::with_capacity(missing.len());
    for directory in missing {
        match new_private_dir().create(directory) {
            Ok(()) => {}
            // Lost a race with another process. It is not ours, so it is
            // neither sealed nor claimed.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
        seal_owner_only(directory)?;
        created.push(directory.to_path_buf());
    }
    Ok(created)
}

/// Set one existing directory to [`OWNER_ONLY`].
///
/// # Errors
///
/// The `chmod` failure, verbatim.
pub fn seal_owner_only(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(OWNER_ONLY))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn new_private_dir() -> std::fs::DirBuilder {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(OWNER_ONLY);
    }
    builder
}

/// Whether an existing directory is owner-only, and whose it is.
///
/// Separated from creation because the two questions have different answers for
/// a tree pmux did not make: creating one privately is pmux's job, and deciding
/// whether to trust one an operator supplied is the caller's.
#[cfg(unix)]
#[must_use]
pub fn owner_only_violation(metadata: &std::fs::Metadata, effective_uid: u32) -> Option<String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if metadata.uid() != effective_uid {
        return Some(format!(
            "it is owned by uid {}, not the current user",
            metadata.uid()
        ));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Some(format!("its mode is {mode:o}, not owner-only"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every level this call creates is owner-only, not just the last one.
    ///
    /// The regression this module exists for. `create_dir_all` + one `chmod`
    /// passes an assertion written about `deep/run` alone and leaves `deep`
    /// and its parent at `0o755`.
    #[test]
    #[cfg(unix)]
    fn every_level_created_is_owner_only_and_not_only_the_leaf() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let leaf = temp.path().join("one/two/three");
        let created = create_private_dir_all(&leaf).expect("the tree is created");

        assert_eq!(
            created,
            vec![
                temp.path().join("one"),
                temp.path().join("one/two"),
                temp.path().join("one/two/three"),
            ],
            "the created list must name every level, outermost first"
        );
        // Walked from the report rather than from a list written here, so a
        // level this call creates and does not report cannot pass by omission.
        for directory in &created {
            let mode = std::fs::metadata(directory)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                OWNER_ONLY,
                "{} is not owner-only",
                directory.display()
            );
        }
    }

    /// A directory that already existed is neither re-permissioned nor claimed.
    #[test]
    #[cfg(unix)]
    fn an_existing_directory_is_left_alone_and_not_reported_as_created() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let existing = temp.path().join("operator");
        std::fs::create_dir(&existing).expect("operator directory");
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755))
            .expect("operator mode");

        let created = create_private_dir_all(&existing).expect("nothing to create");
        assert!(created.is_empty(), "{created:?}");
        assert_eq!(
            std::fs::metadata(&existing)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o755,
            "this function does not re-permission a tree it did not make"
        );

        // ...and a child of it is still created privately.
        let child = existing.join("child");
        assert_eq!(
            create_private_dir_all(&child).expect("child is created"),
            vec![child.clone()]
        );
        assert_eq!(
            std::fs::metadata(&child)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            OWNER_ONLY
        );
    }

    #[test]
    #[cfg(unix)]
    fn a_wide_mode_and_a_foreign_owner_are_both_named() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let temp = tempfile::tempdir().expect("tempdir");
        let directory = temp.path().join("wide");
        create_private_dir_all(&directory).expect("created");
        let metadata = std::fs::metadata(&directory).expect("metadata");
        let uid = metadata.uid();
        assert_eq!(owner_only_violation(&metadata, uid), None);
        assert!(
            owner_only_violation(&metadata, uid.wrapping_add(1))
                .is_some_and(|reason| reason.contains("owned by uid")),
            "a directory owned by someone else must be named as such"
        );

        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o750))
            .expect("widen");
        let metadata = std::fs::metadata(&directory).expect("metadata");
        assert!(
            owner_only_violation(&metadata, uid).is_some_and(|reason| reason.contains("750")),
            "a group-readable directory must be named as such"
        );
    }
}
