#![cfg(unix)]

#[allow(dead_code)]
mod support;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use support::{bind_run_and_revalidate_pmux_candidate_for_test, resolve_pmux_candidate_for_test};

fn make_executable(path: &Path) {
    fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn exact_directory(root: &Path) -> PathBuf {
    let directory = root.join("candidate");
    fs::create_dir(&directory).unwrap();
    directory.canonicalize().unwrap()
}

#[test]
fn exact_directory_binds_a_direct_regular_executable() {
    let root = tempfile::tempdir().unwrap();
    let directory = exact_directory(root.path());
    let candidate = directory.join("pmux");
    make_executable(&candidate);

    assert_eq!(
        resolve_pmux_candidate_for_test(
            Some(&directory),
            Path::new("/unused-cargo-pmux-candidate"),
        )
        .unwrap(),
        candidate
    );
}

#[test]
fn cargo_fallback_binds_its_absolute_executable() {
    let root = tempfile::tempdir().unwrap();
    let candidate = root.path().join("pmux");
    make_executable(&candidate);
    let candidate = candidate.canonicalize().unwrap();

    assert_eq!(
        resolve_pmux_candidate_for_test(None, &candidate).unwrap(),
        candidate
    );
}

#[test]
fn exact_directory_rejects_relative_and_missing_candidates() {
    let error = resolve_pmux_candidate_for_test(
        Some(Path::new("relative/candidate")),
        Path::new("/unused-cargo-pmux-candidate"),
    )
    .unwrap_err();
    assert!(error.contains("must be absolute"), "{error}");

    let root = tempfile::tempdir().unwrap();
    let directory = exact_directory(root.path());
    let error = resolve_pmux_candidate_for_test(
        Some(&directory),
        Path::new("/unused-cargo-pmux-candidate"),
    )
    .unwrap_err();
    assert!(
        error.contains("required exact pmux candidate is unavailable"),
        "{error}"
    );
}

#[test]
fn exact_directory_rejects_escaped_and_aliased_candidates() {
    let root = tempfile::tempdir().unwrap();
    let directory = exact_directory(root.path());
    let outside = root.path().join("outside-pmux");
    make_executable(&outside);
    symlink(&outside, directory.join("pmux")).unwrap();

    let error = resolve_pmux_candidate_for_test(
        Some(&directory),
        Path::new("/unused-cargo-pmux-candidate"),
    )
    .unwrap_err();
    assert!(error.contains("escaped exact binary directory"), "{error}");

    fs::remove_file(directory.join("pmux")).unwrap();
    let direct = directory.join("direct-pmux");
    make_executable(&direct);
    symlink(&direct, directory.join("pmux")).unwrap();

    let error = resolve_pmux_candidate_for_test(
        Some(&directory),
        Path::new("/unused-cargo-pmux-candidate"),
    )
    .unwrap_err();
    assert!(error.contains("not an alias"), "{error}");
}

#[test]
fn exact_directory_rejects_non_regular_or_non_executable_candidates() {
    let root = tempfile::tempdir().unwrap();
    let directory = exact_directory(root.path());
    let candidate = directory.join("pmux");
    fs::create_dir(&candidate).unwrap();

    let error = resolve_pmux_candidate_for_test(
        Some(&directory),
        Path::new("/unused-cargo-pmux-candidate"),
    )
    .unwrap_err();
    assert!(error.contains("not a direct regular file"), "{error}");

    fs::remove_dir(&candidate).unwrap();
    fs::write(&candidate, b"not executable").unwrap();
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600)).unwrap();

    let error = resolve_pmux_candidate_for_test(
        Some(&directory),
        Path::new("/unused-cargo-pmux-candidate"),
    )
    .unwrap_err();
    assert!(error.contains("not executable"), "{error}");
}

#[test]
fn post_exit_revalidation_detects_candidate_content_replacement() {
    let root = tempfile::tempdir().unwrap();
    let directory = exact_directory(root.path());
    let candidate = directory.join("pmux");
    make_executable(&candidate);

    let error = bind_run_and_revalidate_pmux_candidate_for_test(
        Some(&directory),
        Path::new("/unused-cargo-pmux-candidate"),
        |bound| {
            let status = std::process::Command::new(bound).status().unwrap();
            assert!(status.success());
            fs::write(bound, b"#!/bin/sh\nexit 1\n").unwrap();
        },
    )
    .unwrap_err();
    assert!(
        error.contains("changed regular executable identity"),
        "{error}"
    );
}
