#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use sha2::{Digest, Sha256};

const UPSTREAM_MANIFEST: &str = include_str!("fixtures/rmux-client-0.9.0.sha256");
const CRATE_NAME: &str = "rmux-client";
const CRATE_VERSION: &str = "0.9.0";
const ARCHIVE_SHA256: &str = "0229231128141add0463cd755b03ce29e3057086555f893cbe52d36705aefe3f";
const VCS_SHA1: &str = "b2f80522bae2927e22d81e5c902b727623f934d0";
const UPSTREAM_ATTACH_SHA256: &str =
    "ff4721284bd8941f59e2a6cf57850fde37e87bee365eab8f283a1e77bc8c0bb3";
const PATCHED_ATTACH_SHA256: &str =
    "099b2218021e508b376164659ccfb8ad1d043c685f7555b7026af81a71401164";
const UPSTREAM_DECODE_CALL: &[u8] = b"decode_attach_data_frame(&read_buffer[consumed..])";
const PATCHED_DECODE_CALL: &[u8] = b"decode_attach_data_frame(&read_buffer[consumed..bytes_read])";

#[derive(Debug)]
struct PublishedManifest {
    metadata: BTreeMap<String, String>,
    files: BTreeMap<String, String>,
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must resolve")
}

fn vendor_root() -> PathBuf {
    workspace_root().join("vendor/rmux-client")
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn parse_published_manifest() -> PublishedManifest {
    let mut metadata = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut parsing_files = false;
    for (index, line) in UPSTREAM_MANIFEST.lines().enumerate() {
        if line == "---" {
            assert!(!parsing_files, "duplicate manifest separator");
            parsing_files = true;
            continue;
        }
        assert!(!line.is_empty(), "blank manifest line {}", index + 1);
        if parsing_files {
            let (digest, path) = line
                .split_once("  ")
                .unwrap_or_else(|| panic!("invalid file row on line {}", index + 1));
            assert_eq!(digest.len(), 64, "invalid SHA-256 on line {}", index + 1);
            assert!(
                digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "non-hex SHA-256 on line {}",
                index + 1
            );
            assert!(!path.is_empty(), "empty path on line {}", index + 1);
            assert!(
                files.insert(path.to_owned(), digest.to_owned()).is_none(),
                "duplicate upstream path {path}"
            );
        } else {
            let (key, value) = line
                .split_once('=')
                .unwrap_or_else(|| panic!("invalid metadata row on line {}", index + 1));
            assert!(
                metadata.insert(key.to_owned(), value.to_owned()).is_none(),
                "duplicate manifest metadata key {key}"
            );
        }
    }
    assert!(parsing_files, "manifest file section is missing");
    PublishedManifest { metadata, files }
}

fn collect_vendor_files(root: &Path, directory: &Path, files: &mut BTreeMap<String, PathBuf>) {
    for entry in fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", directory.display()))
    {
        let entry = entry.expect("vendor directory entry must be readable");
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .unwrap_or_else(|error| panic!("could not stat {}: {error}", path.display()));
        assert!(
            !metadata.file_type().is_symlink(),
            "vendor tree contains symlink {}",
            path.display()
        );
        if metadata.is_dir() {
            collect_vendor_files(root, &path, files);
        } else {
            assert!(
                metadata.is_file(),
                "vendor tree contains special node {}",
                path.display()
            );
            assert_eq!(
                metadata.permissions().mode() & 0o111,
                0,
                "published source file unexpectedly became executable: {}",
                path.display()
            );
            let relative = path
                .strip_prefix(root)
                .expect("vendor file must remain below root")
                .to_str()
                .expect("published vendor paths must be UTF-8")
                .replace(std::path::MAIN_SEPARATOR, "/");
            assert!(
                files.insert(relative.clone(), path).is_none(),
                "duplicate vendor path {relative}"
            );
        }
    }
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn reconstruct_upstream_attach(patched: &[u8]) -> Vec<u8> {
    assert_eq!(
        count_occurrences(patched, PATCHED_DECODE_CALL),
        1,
        "the documented patched call must occur exactly once"
    );
    assert_eq!(
        count_occurrences(patched, UPSTREAM_DECODE_CALL),
        0,
        "the unsafe upstream call must not remain"
    );
    let offset = patched
        .windows(PATCHED_DECODE_CALL.len())
        .position(|window| window == PATCHED_DECODE_CALL)
        .expect("patched call occurrence was counted above");
    let mut reconstructed =
        Vec::with_capacity(patched.len() - PATCHED_DECODE_CALL.len() + UPSTREAM_DECODE_CALL.len());
    reconstructed.extend_from_slice(&patched[..offset]);
    reconstructed.extend_from_slice(UPSTREAM_DECODE_CALL);
    reconstructed.extend_from_slice(&patched[offset + PATCHED_DECODE_CALL.len()..]);
    reconstructed
}

#[test]
fn vendored_tree_is_the_published_archive_plus_exactly_one_documented_line() {
    let published = parse_published_manifest();
    assert_eq!(published.metadata.len(), 6);
    assert_eq!(
        published.metadata.get("schema").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        published.metadata.get("crate").map(String::as_str),
        Some(CRATE_NAME)
    );
    assert_eq!(
        published.metadata.get("version").map(String::as_str),
        Some(CRATE_VERSION)
    );
    assert_eq!(
        published.metadata.get("archive_sha256").map(String::as_str),
        Some(ARCHIVE_SHA256)
    );
    assert_eq!(
        published.metadata.get("vcs_sha1").map(String::as_str),
        Some(VCS_SHA1)
    );
    let declared_count = published
        .metadata
        .get("file_count")
        .expect("file_count metadata is required")
        .parse::<usize>()
        .expect("file_count must be an integer");
    assert_eq!(declared_count, 63);
    assert_eq!(published.files.len(), declared_count);
    assert_eq!(
        published.files.get("src/attach.rs").map(String::as_str),
        Some(UPSTREAM_ATTACH_SHA256)
    );

    let root = vendor_root();
    let mut actual = BTreeMap::new();
    collect_vendor_files(&root, &root, &mut actual);
    let patch_document = actual
        .remove("PMUX-PATCH.md")
        .expect("the local patch document is required");
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        published.files.keys().collect::<Vec<_>>(),
        "vendor paths must equal the published archive; Cargo extraction markers are forbidden"
    );

    for (relative, expected_digest) in &published.files {
        let path = actual.get(relative).expect("path sets matched above");
        let bytes = fs::read(path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
        if relative == "src/attach.rs" {
            assert_eq!(sha256(&bytes), PATCHED_ATTACH_SHA256);
            assert_eq!(
                sha256(&reconstruct_upstream_attach(&bytes)),
                *expected_digest
            );
        } else {
            assert_eq!(
                sha256(&bytes),
                *expected_digest,
                "vendored upstream file changed: {relative}"
            );
        }
    }

    let vcs: Value = serde_json::from_slice(
        &fs::read(root.join(".cargo_vcs_info.json")).expect("VCS identity must be readable"),
    )
    .expect("VCS identity must be valid JSON");
    assert_eq!(vcs["git"]["sha1"].as_str(), Some(VCS_SHA1));

    let patch_document =
        fs::read_to_string(patch_document).expect("patch document must be UTF-8 text");
    for required in [
        ARCHIVE_SHA256,
        VCS_SHA1,
        "&read_buffer[consumed..]",
        "&read_buffer[consumed..bytes_read]",
        "tests/attach_fragmentation.rs",
    ] {
        assert!(
            patch_document.contains(required),
            "patch document is missing {required}"
        );
    }
}

#[test]
fn locked_cargo_graph_resolves_exactly_the_vendored_rmux_client() {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = Command::new(cargo)
        .args(["metadata", "--offline", "--locked", "--format-version", "1"])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata must launch");
    assert!(
        output.status.success(),
        "locked offline cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata must be valid JSON");
    let packages = metadata["packages"]
        .as_array()
        .expect("metadata packages must be an array");
    let rmux_packages = packages
        .iter()
        .filter(|package| package["name"].as_str() == Some(CRATE_NAME))
        .collect::<Vec<_>>();
    assert_eq!(
        rmux_packages.len(),
        1,
        "exactly one rmux-client must resolve"
    );
    let rmux = rmux_packages[0];
    assert_eq!(rmux["version"].as_str(), Some(CRATE_VERSION));
    assert!(
        rmux.get("source").is_some_and(Value::is_null),
        "rmux-client must be a path package"
    );
    let expected_manifest = vendor_root()
        .join("Cargo.toml")
        .canonicalize()
        .expect("vendor manifest must resolve");
    let resolved_manifest = PathBuf::from(
        rmux["manifest_path"]
            .as_str()
            .expect("rmux-client manifest_path must be text"),
    )
    .canonicalize()
    .expect("resolved rmux-client manifest must exist");
    assert_eq!(resolved_manifest, expected_manifest);
    let rmux_id = rmux["id"].as_str().expect("rmux-client ID must be text");
    assert!(
        !metadata["workspace_members"]
            .as_array()
            .expect("workspace_members must be an array")
            .iter()
            .any(|member| member.as_str() == Some(rmux_id)),
        "the pristine vendored package must remain excluded from workspace mutation"
    );

    let consumer = packages
        .iter()
        .find(|package| package["name"].as_str() == Some("pseudomux-rmux"))
        .expect("pseudomux-rmux package must resolve");
    let dependency = consumer["dependencies"]
        .as_array()
        .expect("consumer dependencies must be an array")
        .iter()
        .find(|dependency| dependency["name"].as_str() == Some(CRATE_NAME))
        .expect("pseudomux-rmux must declare rmux-client");
    assert_eq!(dependency["req"].as_str(), Some("=0.9.0"));

    let consumer_id = consumer["id"]
        .as_str()
        .expect("pseudomux-rmux ID must be text");
    let consumer_node = metadata["resolve"]["nodes"]
        .as_array()
        .expect("resolve nodes must be an array")
        .iter()
        .find(|node| node["id"].as_str() == Some(consumer_id))
        .expect("pseudomux-rmux resolve node must exist");
    let resolved_dependency = consumer_node["deps"]
        .as_array()
        .expect("resolve deps must be an array")
        .iter()
        .find(|dependency| dependency["name"].as_str() == Some("rmux_client"))
        .expect("resolved rmux-client dependency must exist");
    assert_eq!(resolved_dependency["pkg"].as_str(), Some(rmux_id));
}
