#![cfg(unix)]

//! Path A process-boundary coverage here is historical.
//!
//! Public dispatch refuses `start_session` / `run_once` / agents. The living
//! product e2e is `pool_concurrency.rs`; the private path table is
//! `cross_cell_contamination.rs`. `tools/dev/check.sh --push` is the living
//! invocation. This file keeps compile-checked client-asset contracts, including
//! the Messages surface, and does not claim to be the product lane.

use std::collections::BTreeSet;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tempfile::TempDir;

const TYPESCRIPT_DIST_PACKAGE: &[u8] = b"{\"type\":\"module\"}\n";

const TYPESCRIPT_CLIENT_ASSETS: &[&str] = &[
    "package.json",
    "src/client.ts",
    "src/index.ts",
    "src/messages.ts",
    "src/protocol.ts",
    "tests/dist-stage.mjs",
    "dist/client.d.ts",
    "dist/client.d.ts.map",
    "dist/client.js",
    "dist/client.js.map",
    "dist/index.d.ts",
    "dist/index.d.ts.map",
    "dist/index.js",
    "dist/index.js.map",
    "dist/messages.d.ts",
    "dist/messages.d.ts.map",
    "dist/messages.js",
    "dist/messages.js.map",
    "dist/package.json",
    "dist/protocol.d.ts",
    "dist/protocol.d.ts.map",
    "dist/protocol.js",
    "dist/protocol.js.map",
];
const PYTHON_CLIENT_ASSETS: &[&str] = &[
    "pyproject.toml",
    "pmux_client/__init__.py",
    "pmux_client/client.py",
    "pmux_client/messages.py",
    "pmux_client/protocol.py",
    "pmux_client/py.typed",
];

#[test]
fn client_asset_manifests_include_the_messages_surface() {
    assert!(
        TYPESCRIPT_CLIENT_ASSETS.contains(&"src/messages.ts"),
        "TypeScript Messages client must stay on the staged source list"
    );
    assert!(
        TYPESCRIPT_CLIENT_ASSETS.contains(&"dist/messages.js"),
        "TypeScript Messages client must stay on the staged dist list"
    );
    assert!(
        TYPESCRIPT_CLIENT_ASSETS.contains(&"dist/messages.d.ts"),
        "TypeScript Messages types must stay on the staged dist list"
    );
    assert!(
        PYTHON_CLIENT_ASSETS.contains(&"pmux_client/messages.py"),
        "Python Messages client must stay on the staged source list"
    );

    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    assert!(
        workspace
            .join("clients/typescript/src/messages.ts")
            .is_file()
    );
    assert!(
        workspace
            .join("clients/python/pmux_client/messages.py")
            .is_file()
    );
}

#[test]
fn external_typescript_stage_contract_rejects_invalid_roots_membership_modes_and_aliases() {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let valid = typescript_dist_fixture();
    let valid_root = valid.path().canonicalize().unwrap();
    assert_eq!(
        validate_external_directory(&workspace, "test TypeScript dist", &valid_root),
        valid_root
    );
    validate_typescript_dist_root(&valid_root);

    assert_panics(|| {
        validate_external_directory(
            &workspace,
            "relative TypeScript dist",
            Path::new("relative/dist"),
        );
    });
    let noncanonical = valid_root.join("..").join(valid_root.file_name().unwrap());
    assert_panics(|| {
        validate_external_directory(&workspace, "noncanonical TypeScript dist", &noncanonical);
    });
    assert_panics(|| {
        validate_external_directory(
            &workspace,
            "in-workspace TypeScript dist",
            &workspace.join("clients/typescript/tests"),
        );
    });

    let link_parent = tempfile::tempdir().unwrap();
    let linked_root = link_parent.path().join("linked-dist");
    std::os::unix::fs::symlink(&valid_root, &linked_root).unwrap();
    assert_panics(|| {
        validate_external_directory(&workspace, "symlink TypeScript dist", &linked_root);
    });

    for mutation in [
        "missing",
        "extra",
        "mode",
        "hardlink",
        "directory",
        "symlink",
    ] {
        let fixture = typescript_dist_fixture();
        let root = fixture.path().canonicalize().unwrap();
        match mutation {
            "missing" => std::fs::remove_file(root.join("client.js")).unwrap(),
            "extra" => write_private_file(&root.join("extra.js"), b"export {};\n"),
            "mode" => std::fs::set_permissions(
                root.join("client.js"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap(),
            "hardlink" => {
                std::fs::remove_file(root.join("client.js")).unwrap();
                std::fs::hard_link(root.join("index.js"), root.join("client.js")).unwrap();
            }
            "directory" => {
                std::fs::remove_file(root.join("client.js")).unwrap();
                std::fs::create_dir(root.join("client.js")).unwrap();
            }
            "symlink" => {
                std::fs::remove_file(root.join("client.js")).unwrap();
                std::os::unix::fs::symlink(root.join("index.js"), root.join("client.js")).unwrap();
            }
            _ => unreachable!(),
        }
        assert_panics(|| validate_typescript_dist_root(&root));
    }
}

fn assert_panics(operation: impl FnOnce() + std::panic::UnwindSafe) {
    assert!(std::panic::catch_unwind(operation).is_err());
}

fn typescript_dist_fixture() -> TempDir {
    let fixture = tempfile::tempdir().unwrap();
    std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    for relative in TYPESCRIPT_CLIENT_ASSETS
        .iter()
        .filter_map(|relative| relative.strip_prefix("dist/"))
    {
        let bytes = if relative == "package.json" {
            TYPESCRIPT_DIST_PACKAGE
        } else {
            b"generated\n"
        };
        write_private_file(&fixture.path().join(relative), bytes);
    }
    fixture
}

fn write_private_file(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn validate_external_directory(workspace: &Path, label: &str, supplied: &Path) -> PathBuf {
    assert!(supplied.is_absolute(), "{label} must be absolute");
    let supplied_metadata = std::fs::symlink_metadata(supplied)
        .unwrap_or_else(|error| panic!("{label} must exist: {error}"));
    assert!(
        !supplied_metadata.file_type().is_symlink() && supplied_metadata.is_dir(),
        "{label} must be a real directory"
    );
    let canonical = supplied
        .canonicalize()
        .unwrap_or_else(|error| panic!("{label} must exist: {error}"));
    assert_eq!(
        supplied, canonical,
        "{label} must name its canonical directory"
    );
    assert!(
        !canonical.starts_with(workspace),
        "{label} must be outside the canonical workspace"
    );
    assert_eq!(
        supplied_metadata.mode() & 0o777,
        0o700,
        "{label} must be owner-private"
    );
    canonical
}

fn validate_typescript_dist_root(root: &Path) {
    let expected = TYPESCRIPT_CLIENT_ASSETS
        .iter()
        .filter_map(|relative| relative.strip_prefix("dist/"))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let actual = std::fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .file_name()
                .into_string()
                .expect("TypeScript dist names must be UTF-8")
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(actual, expected, "TypeScript dist membership changed");

    let mut identities = BTreeSet::new();
    for relative in &expected {
        let path = required_typescript_dist_file(root, relative);
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert!(
            identities.insert((metadata.dev(), metadata.ino())),
            "TypeScript dist file aliases another stage member: {relative}"
        );
    }
    assert_eq!(
        std::fs::read(root.join("package.json")).unwrap(),
        TYPESCRIPT_DIST_PACKAGE,
        "TypeScript dist package scope changed"
    );
}

fn required_typescript_dist_file(root: &Path, relative: &str) -> PathBuf {
    let unresolved = root.join(relative);
    let metadata = std::fs::symlink_metadata(&unresolved).unwrap_or_else(|error| {
        panic!(
            "required TypeScript dist asset is missing ({}): {error}",
            unresolved.display()
        )
    });
    assert!(
        !metadata.file_type().is_symlink() && metadata.is_file(),
        "TypeScript dist asset must be a regular file: {}",
        unresolved.display()
    );
    assert_eq!(metadata.nlink(), 1, "TypeScript dist asset is hard-linked");
    assert_eq!(
        metadata.mode() & 0o777,
        0o600,
        "TypeScript dist asset must be owner-private"
    );
    let path = unresolved.canonicalize().unwrap();
    assert_eq!(path.parent(), Some(root));
    path
}
