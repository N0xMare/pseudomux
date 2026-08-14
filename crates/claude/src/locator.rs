use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde_json::Value;
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

const MAX_PROJECT_DIRECTORIES: usize = 10_000;
const MAX_VALIDATION_LINES: usize = 256;
const MAX_VALIDATION_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
struct LocatorLimits {
    project_directories: usize,
    validation_lines: usize,
    validation_bytes: u64,
}

impl Default for LocatorLimits {
    fn default() -> Self {
        Self {
            project_directories: MAX_PROJECT_DIRECTORIES,
            validation_lines: MAX_VALIDATION_LINES,
            validation_bytes: MAX_VALIDATION_BYTES,
        }
    }
}

/// A transcript file whose early records match the requested Claude identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocatedTranscript {
    pub path: PathBuf,
    pub project_directory: PathBuf,
}

#[derive(Debug, Error)]
pub enum TranscriptLocationError {
    #[error("Claude configuration root must be absolute: {0}")]
    RelativeConfigRoot(PathBuf),
    #[error("Claude cwd is unavailable: {path}: {source}")]
    InvalidCwd {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to inspect Claude transcript storage at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("transcript scan exceeded {MAX_PROJECT_DIRECTORIES} project directories")]
    ScanLimit,
    #[error("no validated main transcript exists yet for session {session_id}")]
    NotFound { session_id: String },
    #[error("multiple validated transcripts exist for session {session_id}: {paths:?}")]
    Ambiguous {
        session_id: String,
        paths: Vec<PathBuf>,
    },
}

/// Bounded locator for `projects/<project>/<session>.jsonl` beneath the effective config root.
///
/// A locator is bound to one config root and one cwd, not to one session id.
/// The cwd is what selects the project directory, and a live Claude session can
/// rotate its session id without ever leaving that directory: `/clear` abandons
/// the current transcript and opens a new `<new-uuid>.jsonl` beside it. So the
/// session id travels per call, and [`TranscriptLocator::new`] records one only
/// as the identity for the launch-time admission checks that are made once,
/// against an id that cannot change underneath them.
#[derive(Clone, Debug)]
pub struct TranscriptLocator {
    config_root: PathBuf,
    projects_root: PathBuf,
    canonical_cwd: PathBuf,
    normalized_cwd: String,
    session_id: String,
    limits: LocatorLimits,
}

impl TranscriptLocator {
    pub fn new(
        config_root: impl Into<PathBuf>,
        cwd: impl AsRef<Path>,
        session_id: impl Into<String>,
    ) -> Result<Self, TranscriptLocationError> {
        let config_root = config_root.into();
        if !config_root.is_absolute() {
            return Err(TranscriptLocationError::RelativeConfigRoot(config_root));
        }
        let canonical_cwd =
            cwd.as_ref()
                .canonicalize()
                .map_err(|source| TranscriptLocationError::InvalidCwd {
                    path: cwd.as_ref().to_path_buf(),
                    source,
                })?;
        let normalized_cwd = normalize_path(&canonical_cwd);
        let projects_root = config_root.join("projects");
        Ok(Self {
            config_root,
            projects_root,
            canonical_cwd,
            normalized_cwd,
            session_id: session_id.into(),
            limits: LocatorLimits::default(),
        })
    }

    #[cfg(test)]
    fn with_limits(mut self, limits: LocatorLimits) -> Self {
        self.limits = limits;
        self
    }

    #[must_use]
    pub fn config_root(&self) -> &Path {
        &self.config_root
    }

    #[must_use]
    pub fn canonical_cwd(&self) -> &Path {
        &self.canonical_cwd
    }

    /// Returns deterministic fast-path candidates before bounded fallback scanning.
    #[must_use]
    pub fn expected_candidates(&self, session_id: &str) -> Vec<PathBuf> {
        let filename = format!("{session_id}.jsonl");
        let slug = sanitize_project_path(&self.normalized_cwd);
        let mut paths = vec![self.projects_root.join(&slug).join(&filename)];

        // Some Claude/Bun and SDK releases disagree on whether repeated separators
        // collapse. The fallback scan remains authoritative, but both cheap forms
        // avoid a full directory walk for ordinary paths.
        let collapsed = collapse_dashes(&slug);
        if collapsed != slug {
            paths.push(self.projects_root.join(collapsed).join(filename));
        }
        paths
    }

    /// Finds exactly one transcript whose contents corroborate both UUID and cwd.
    ///
    /// Uses the identity supplied at construction. Callers that follow a live
    /// session must use [`Self::locate_for`] instead: an id fixed at
    /// construction cannot survive the session rotating it.
    pub fn locate(&self) -> Result<LocatedTranscript, TranscriptLocationError> {
        self.locate_for(&self.session_id)
    }

    /// Finds exactly one transcript corroborating `session_id` and this
    /// locator's cwd.
    pub fn locate_for(
        &self,
        session_id: &str,
    ) -> Result<LocatedTranscript, TranscriptLocationError> {
        let mut matches = Vec::new();
        for candidate in self.existing_session_files_for(session_id)? {
            if self.validate_candidate(session_id, &candidate)? {
                push_unique(&mut matches, candidate);
            }
        }

        match matches.as_slice() {
            [] => Err(TranscriptLocationError::NotFound {
                session_id: session_id.to_owned(),
            }),
            [path] => Ok(LocatedTranscript {
                project_directory: path.parent().unwrap_or(&self.projects_root).to_path_buf(),
                path: path.clone(),
            }),
            paths => Err(TranscriptLocationError::Ambiguous {
                session_id: session_id.to_owned(),
                paths: paths.to_vec(),
            }),
        }
    }

    /// Finds every existing `projects/*/<session>.jsonl` filename, regardless of
    /// project cwd. New-session admission uses this stricter collision check so a
    /// caller-supplied Claude UUID cannot silently alias history in another
    /// project. Resume still requires [`Self::locate`] to validate UUID and cwd
    /// from transcript contents.
    ///
    /// Uses the identity supplied at construction, which is what admission
    /// wants: the collision question is asked once, about the id the launch was
    /// resolved with.
    pub fn existing_session_files(&self) -> Result<Vec<PathBuf>, TranscriptLocationError> {
        self.existing_session_files_for(&self.session_id)
    }

    /// Finds every existing `projects/*/<session_id>.jsonl` filename.
    pub fn existing_session_files_for(
        &self,
        session_id: &str,
    ) -> Result<Vec<PathBuf>, TranscriptLocationError> {
        let mut matches = Vec::new();
        for candidate in self.expected_candidates(session_id) {
            if candidate.is_file() {
                push_unique(&mut matches, candidate);
            }
        }

        if self.projects_root.is_dir() {
            let entries = std::fs::read_dir(&self.projects_root).map_err(|source| {
                TranscriptLocationError::Io {
                    path: self.projects_root.clone(),
                    source,
                }
            })?;
            let mut scanned_directories = 0;
            for entry in entries {
                let entry = entry.map_err(|source| TranscriptLocationError::Io {
                    path: self.projects_root.clone(),
                    source,
                })?;
                if !entry
                    .file_type()
                    .map_err(|source| TranscriptLocationError::Io {
                        path: entry.path(),
                        source,
                    })?
                    .is_dir()
                {
                    continue;
                }
                if scanned_directories >= self.limits.project_directories {
                    return Err(TranscriptLocationError::ScanLimit);
                }
                scanned_directories += 1;
                let candidate = entry.path().join(format!("{session_id}.jsonl"));
                if candidate.is_file() {
                    push_unique(&mut matches, candidate);
                }
            }
        }
        matches.sort_unstable();
        Ok(matches)
    }

    fn validate_candidate(
        &self,
        session_id: &str,
        path: &Path,
    ) -> Result<bool, TranscriptLocationError> {
        let file = File::open(path).map_err(|source| TranscriptLocationError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut reader = BufReader::new(file).take(self.limits.validation_bytes);
        let mut line = Vec::new();
        for _ in 0..self.limits.validation_lines {
            line.clear();
            let bytes_read = reader.read_until(b'\n', &mut line).map_err(|source| {
                TranscriptLocationError::Io {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
            if bytes_read == 0 {
                break;
            }

            // Identity admission has the same complete-record boundary as the
            // transcript tailer. `Read::take` can stop exactly after a valid
            // JSON object but before its newline; accepting that prefix would
            // let a byte-boundary or unterminated row authorize a resume before
            // the authoritative tailer has observed one complete JSONL record.
            if line.last() != Some(&b'\n') {
                break;
            }
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }

            let Ok(row) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            // Identity must be corroborated atomically by one transcript row.
            // Accepting a session ID from one row and a cwd from another lets a
            // malformed or spliced file satisfy resume admission without any
            // real Claude record carrying the requested identity pair.
            let session_matches = row.get("sessionId").and_then(Value::as_str) == Some(session_id);
            let cwd_matches = row
                .get("cwd")
                .and_then(Value::as_str)
                .is_some_and(|cwd| normalize_candidate_cwd(cwd) == self.normalized_cwd);
            if session_matches && cwd_matches {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn normalize_candidate_cwd(value: &str) -> String {
    let path = Path::new(value);
    path.canonicalize()
        .map_or_else(|_| value.nfc().collect(), |path| normalize_path(&path))
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().nfc().collect()
}

fn sanitize_project_path(path: &str) -> String {
    path.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn collapse_dashes(value: &str) -> String {
    let mut previous_dash = false;
    value
        .chars()
        .filter(|character| {
            let include = *character != '-' || !previous_dash;
            previous_dash = *character == '-';
            include
        })
        .collect()
}

fn push_unique(values: &mut Vec<PathBuf>, path: PathBuf) {
    if !values.contains(&path) {
        values.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::{
        prelude::*,
        test_runner::{Config as ProptestConfig, RngAlgorithm, RngSeed},
    };
    use std::io::Write;

    #[test]
    fn expected_path_and_bounded_scan_find_validated_transcript() {
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let config = root.path().join("claude");
        let project = config.join("projects").join("unexpected-hash-form");
        std::fs::create_dir_all(&project).unwrap();
        let session_id = "00000000-0000-4000-8000-000000000001";
        let transcript = project.join(format!("{session_id}.jsonl"));
        let mut file = File::create(&transcript).unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "type": "user",
                "uuid": "user-1",
                "sessionId": session_id,
                "cwd": cwd.path().canonicalize().unwrap(),
                "promptSource": "typed",
                "message": {"content": "hello"}
            })
        )
        .unwrap();

        let located =
            TranscriptLocator::new(config.canonicalize().unwrap(), cwd.path(), session_id)
                .unwrap()
                .locate()
                .unwrap();
        assert_eq!(located.path, transcript.canonicalize().unwrap());
    }

    #[test]
    fn one_locator_follows_a_session_that_rotates_its_id() {
        // A session can rotate its id without leaving its project directory:
        // `/clear` abandons the current transcript and opens a new
        // `<new-uuid>.jsonl` beside it. The abandoned file keeps its inode and
        // its length forever, so a locator that could only ever answer for the
        // construction-bound id would keep resolving to a file that will never
        // grow again.
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let canonical_cwd = cwd.path().canonicalize().unwrap();
        let config = root.path().join("claude");
        let project = config.join("projects").join("project");
        std::fs::create_dir_all(&project).unwrap();
        let launch_session = "00000000-0000-4000-8000-000000000001";
        let rotated_session = "00000000-0000-4000-8000-000000000002";
        let write_transcript = |session_id: &str| {
            let path = project.join(format!("{session_id}.jsonl"));
            std::fs::write(
                &path,
                serde_json::json!({"sessionId": session_id, "cwd": canonical_cwd}).to_string()
                    + "\n",
            )
            .unwrap();
            path.canonicalize().unwrap()
        };
        let launch_transcript = write_transcript(launch_session);

        let locator =
            TranscriptLocator::new(config.canonicalize().unwrap(), cwd.path(), launch_session)
                .unwrap();
        // The requested id, not the bound one, decides. Before the rotation
        // exists on disk the answer must be not-found rather than the launch
        // transcript.
        assert!(matches!(
            locator.locate_for(rotated_session),
            Err(TranscriptLocationError::NotFound { .. })
        ));

        let rotated_transcript = write_transcript(rotated_session);
        assert_eq!(
            locator.locate_for(rotated_session).unwrap().path,
            rotated_transcript
        );
        assert_eq!(
            locator.existing_session_files_for(rotated_session).unwrap(),
            vec![rotated_transcript]
        );
        // The construction-bound forms are unmoved. Launch admission asks its
        // question once, about the id the launch was resolved with, and must not
        // start answering for whatever the session rotated to afterwards.
        assert_eq!(
            locator.locate_for(launch_session).unwrap().path,
            launch_transcript
        );
        assert_eq!(locator.locate().unwrap().path, launch_transcript);
        assert_eq!(
            locator.existing_session_files().unwrap(),
            vec![launch_transcript]
        );
    }

    #[test]
    fn fast_path_candidates_name_the_requested_session() {
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let canonical_cwd = cwd.path().canonicalize().unwrap();
        let config = root.path().join("claude");
        std::fs::create_dir_all(&config).unwrap();
        let config = config.canonicalize().unwrap();
        let launch_session = "00000000-0000-4000-8000-000000000001";
        let rotated_session = "00000000-0000-4000-8000-000000000002";
        let slug = sanitize_project_path(&normalize_path(&canonical_cwd));
        let project = config.join("projects").join(&slug);
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(format!("{rotated_session}.jsonl")),
            serde_json::json!({"sessionId": rotated_session, "cwd": canonical_cwd}).to_string()
                + "\n",
        )
        .unwrap();

        let locator = TranscriptLocator::new(&config, cwd.path(), launch_session).unwrap();
        // The deterministic candidate is what keeps ordinary lookups off the
        // directory walk, so it has to move with the requested id too.
        assert!(
            locator
                .expected_candidates(rotated_session)
                .contains(&project.join(format!("{rotated_session}.jsonl")))
        );
        assert!(
            !locator
                .expected_candidates(rotated_session)
                .iter()
                .any(|candidate| candidate.ends_with(format!("{launch_session}.jsonl")))
        );
        assert_eq!(
            locator.locate_for(rotated_session).unwrap().path,
            project
                .join(format!("{rotated_session}.jsonl"))
                .canonicalize()
                .unwrap()
        );
    }

    #[test]
    fn filename_without_matching_content_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let config = root.path().join("claude");
        let project = config.join("projects").join("project");
        std::fs::create_dir_all(&project).unwrap();
        let session_id = "00000000-0000-4000-8000-000000000001";
        std::fs::write(
            project.join(format!("{session_id}.jsonl")),
            serde_json::json!({"sessionId": "other", "cwd": cwd.path()}).to_string() + "\n",
        )
        .unwrap();
        let error = TranscriptLocator::new(config.canonicalize().unwrap(), cwd.path(), session_id)
            .unwrap()
            .locate()
            .unwrap_err();
        assert!(matches!(error, TranscriptLocationError::NotFound { .. }));
    }

    #[test]
    fn identity_fields_split_across_rows_do_not_validate_resume() {
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let config = root.path().join("claude");
        let project = config.join("projects").join("project");
        std::fs::create_dir_all(&project).unwrap();
        let session_id = "00000000-0000-4000-8000-000000000001";
        let transcript = project.join(format!("{session_id}.jsonl"));
        let canonical_cwd = cwd.path().canonicalize().unwrap();
        std::fs::write(
            transcript,
            format!(
                "{}\n{}\n",
                serde_json::json!({"sessionId": session_id}),
                serde_json::json!({"cwd": canonical_cwd}),
            ),
        )
        .unwrap();

        let error = TranscriptLocator::new(config.canonicalize().unwrap(), cwd.path(), session_id)
            .unwrap()
            .locate()
            .unwrap_err();
        assert!(matches!(error, TranscriptLocationError::NotFound { .. }));
    }

    #[test]
    fn collision_scan_finds_same_uuid_in_a_different_project() {
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let config = root.path().join("claude");
        let foreign_project = config.join("projects").join("foreign-project");
        std::fs::create_dir_all(&foreign_project).unwrap();
        let session_id = "00000000-0000-4000-8000-000000000001";
        let transcript = foreign_project.join(format!("{session_id}.jsonl"));
        std::fs::write(
            &transcript,
            serde_json::json!({
                "sessionId": session_id,
                "cwd": "/a/different/project"
            })
            .to_string()
                + "\n",
        )
        .unwrap();

        let locator =
            TranscriptLocator::new(config.canonicalize().unwrap(), cwd.path(), session_id).unwrap();
        assert_eq!(
            locator.existing_session_files().unwrap(),
            vec![transcript.canonicalize().unwrap()]
        );
        assert!(matches!(
            locator.locate().unwrap_err(),
            TranscriptLocationError::NotFound { .. }
        ));
    }

    #[test]
    fn duplicate_valid_candidates_are_rejected_as_ambiguous() {
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let config = root.path().join("claude");
        let session_id = "00000000-0000-4000-8000-000000000001";
        let identity = serde_json::json!({
            "sessionId": session_id,
            "cwd": cwd.path().canonicalize().unwrap(),
        })
        .to_string()
            + "\n";

        let mut expected = Vec::new();
        for project_name in ["project-b", "project-a"] {
            let project = config.join("projects").join(project_name);
            std::fs::create_dir_all(&project).unwrap();
            let transcript = project.join(format!("{session_id}.jsonl"));
            std::fs::write(&transcript, &identity).unwrap();
            expected.push(transcript.canonicalize().unwrap());
        }
        expected.sort_unstable();

        let error = TranscriptLocator::new(config.canonicalize().unwrap(), cwd.path(), session_id)
            .unwrap()
            .locate()
            .unwrap_err();
        assert!(matches!(
            error,
            TranscriptLocationError::Ambiguous {
                session_id: ref actual_session,
                paths: ref actual_paths,
            } if actual_session == session_id && actual_paths == &expected
        ));
    }

    #[test]
    fn validation_is_bounded_by_complete_lines_and_bytes() {
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let config = root.path().join("claude");
        let project = config.join("projects").join("project");
        std::fs::create_dir_all(&project).unwrap();
        let session_id = "00000000-0000-4000-8000-000000000001";
        let transcript = project.join(format!("{session_id}.jsonl"));
        let identity = serde_json::json!({
            "sessionId": session_id,
            "cwd": cwd.path().canonicalize().unwrap(),
        })
        .to_string();
        let locator =
            TranscriptLocator::new(config.canonicalize().unwrap(), cwd.path(), session_id)
                .unwrap()
                .with_limits(LocatorLimits {
                    project_directories: 10,
                    validation_lines: 2,
                    validation_bytes: identity.len() as u64,
                });

        std::fs::write(&transcript, format!("{{}}\n{identity}\n")).unwrap();
        assert!(matches!(
            locator.locate(),
            Err(TranscriptLocationError::NotFound { .. })
        ));

        std::fs::write(&transcript, format!("{identity}\nignored\n")).unwrap();
        assert!(matches!(
            locator.locate(),
            Err(TranscriptLocationError::NotFound { .. })
        ));

        let complete_line_bounded = locator.clone().with_limits(LocatorLimits {
            project_directories: 10,
            validation_lines: 2,
            validation_bytes: identity.len() as u64 + 1,
        });
        assert_eq!(
            complete_line_bounded.locate().unwrap().path,
            transcript.canonicalize().unwrap(),
            "identity admission requires the whole object and its JSONL newline"
        );

        let line_bounded = complete_line_bounded.with_limits(LocatorLimits {
            project_directories: 10,
            validation_lines: 2,
            validation_bytes: 1024 * 1024,
        });
        std::fs::write(&transcript, format!("{{}}\n{{}}\n{identity}\n")).unwrap();
        assert!(matches!(
            line_bounded.locate(),
            Err(TranscriptLocationError::NotFound { .. })
        ));
    }

    #[test]
    fn scan_limit_counts_project_directories_not_unrelated_files() {
        let root = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let config = root.path().join("claude");
        let projects = config.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        for index in 0..8 {
            std::fs::write(projects.join(format!("unrelated-{index}")), b"ignored").unwrap();
        }
        std::fs::create_dir(projects.join("only-project")).unwrap();
        let locator = TranscriptLocator::new(
            config.canonicalize().unwrap(),
            cwd.path(),
            "00000000-0000-4000-8000-000000000001",
        )
        .unwrap()
        .with_limits(LocatorLimits {
            project_directories: 1,
            validation_lines: 1,
            validation_bytes: 1,
        });
        assert!(locator.existing_session_files().unwrap().is_empty());

        std::fs::create_dir(projects.join("second-project")).unwrap();
        assert!(matches!(
            locator.existing_session_files(),
            Err(TranscriptLocationError::ScanLimit)
        ));
    }

    #[test]
    fn cwd_identity_uses_canonical_unicode_normalization() {
        let root = tempfile::tempdir().unwrap();
        let cwd_root = tempfile::tempdir().unwrap();
        let cwd = cwd_root.path().join("caf\u{e9}");
        std::fs::create_dir(&cwd).unwrap();
        let canonical_cwd = cwd.canonicalize().unwrap();
        let decomposed_cwd: String = canonical_cwd.to_string_lossy().nfd().collect();

        let config = root.path().join("claude");
        let project = config.join("projects").join("project");
        std::fs::create_dir_all(&project).unwrap();
        let session_id = "00000000-0000-4000-8000-000000000001";
        let transcript = project.join(format!("{session_id}.jsonl"));
        std::fs::write(
            &transcript,
            serde_json::json!({
                "sessionId": session_id,
                "cwd": decomposed_cwd,
            })
            .to_string()
                + "\n",
        )
        .unwrap();

        let located =
            TranscriptLocator::new(config.canonicalize().unwrap(), canonical_cwd, session_id)
                .unwrap()
                .locate()
                .unwrap();
        assert_eq!(located.path, transcript.canonicalize().unwrap());
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 256,
            failure_persistence: None,
            rng_algorithm: RngAlgorithm::ChaCha,
            rng_seed: RngSeed::Fixed(0x504d_5558_4c4f_4341),
            ..ProptestConfig::default()
        })]

        #[test]
        fn generated_split_or_mismatched_identity_rows_never_authorize_resume(
            noise in prop::collection::vec(0_u8..3, 0..24),
            valid_position in 0_usize..25,
            include_valid in any::<bool>(),
        ) {
            let root = tempfile::tempdir().unwrap();
            let cwd = tempfile::tempdir().unwrap();
            let config = root.path().join("claude");
            let project = config.join("projects").join("project");
            std::fs::create_dir_all(&project).unwrap();
            let session_id = "00000000-0000-4000-8000-000000000001";
            let transcript = project.join(format!("{session_id}.jsonl"));
            let canonical_cwd = cwd.path().canonicalize().unwrap();

            let mut rows = Vec::new();
            for mutation in noise {
                rows.push(match mutation {
                    0 => serde_json::json!({
                        "sessionId": session_id,
                        "cwd": "/a/different/project",
                    }),
                    1 => serde_json::json!({
                        "sessionId": "00000000-0000-4000-8000-000000000002",
                        "cwd": canonical_cwd,
                    }),
                    2 => serde_json::json!({
                        "sessionId": session_id,
                    }),
                    _ => unreachable!("generated mutation is bounded"),
                });
            }
            if include_valid {
                let insertion = valid_position.min(rows.len());
                rows.insert(
                    insertion,
                    serde_json::json!({
                        "sessionId": session_id,
                        "cwd": canonical_cwd,
                    }),
                );
            }
            let bytes = rows
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            std::fs::write(&transcript, bytes).unwrap();

            let result = TranscriptLocator::new(
                config.canonicalize().unwrap(),
                cwd.path(),
                session_id,
            )
            .unwrap()
            .locate();
            if include_valid {
                prop_assert_eq!(result.unwrap().path, transcript.canonicalize().unwrap());
            } else {
                prop_assert!(
                    matches!(result, Err(TranscriptLocationError::NotFound { .. })),
                    "mismatched or split identity rows authorized resume"
                );
            }
        }

        #[test]
        fn generated_collision_missing_and_ambiguous_identity_sets_fail_closed(
            scenario in 0_u8..3,
            suffix_a in 0_u16..10_000,
            suffix_b in 0_u16..10_000,
        ) {
            let root = tempfile::tempdir().unwrap();
            let cwd = tempfile::tempdir().unwrap();
            let config = root.path().join("claude");
            let projects = config.join("projects");
            std::fs::create_dir_all(&projects).unwrap();
            let session_id = "00000000-0000-4000-8000-000000000011";
            let canonical_cwd = cwd.path().canonicalize().unwrap();
            let identity = serde_json::json!({
                "sessionId": session_id,
                "cwd": canonical_cwd,
            })
            .to_string()
                + "\n";

            let write_candidate = |project_name: String, contents: &str| {
                let project = projects.join(project_name);
                std::fs::create_dir_all(&project).unwrap();
                let path = project.join(format!("{session_id}.jsonl"));
                std::fs::write(&path, contents).unwrap();
                path.canonicalize().unwrap()
            };

            let locator = TranscriptLocator::new(
                config.canonicalize().unwrap(),
                cwd.path(),
                session_id,
            )
            .unwrap();
            match scenario {
                0 => {
                    let foreign = write_candidate(
                        format!("foreign-{suffix_a}"),
                        &serde_json::json!({
                            "sessionId": session_id,
                            "cwd": "/a/different/project",
                        })
                        .to_string(),
                    );
                    prop_assert_eq!(locator.existing_session_files().unwrap(), vec![foreign]);
                    prop_assert!(matches!(
                        locator.locate(),
                        Err(TranscriptLocationError::NotFound { .. })
                    ), "foreign-project filename must not validate resume");
                }
                1 => {
                    prop_assert!(locator.existing_session_files().unwrap().is_empty());
                    prop_assert!(matches!(
                        locator.locate(),
                        Err(TranscriptLocationError::NotFound { .. })
                    ), "missing resume must remain not-found");
                }
                2 => {
                    let mut expected = vec![
                        write_candidate(format!("valid-a-{suffix_a}"), &identity),
                        write_candidate(format!("valid-b-{suffix_b}"), &identity),
                    ];
                    expected.sort_unstable();
                    prop_assert_eq!(locator.existing_session_files().unwrap(), expected.clone());
                    prop_assert!(matches!(
                        locator.locate(),
                        Err(TranscriptLocationError::Ambiguous { paths, .. })
                            if paths == expected
                    ), "two valid resume files must remain ambiguous");
                }
                _ => unreachable!("generated scenario is bounded"),
            }
        }
    }
}
