//! Conservative Unix process-session observation and teardown.
//!
//! rmux removes terminal state before its background PTY teardown necessarily
//! finishes.  This module therefore treats control-plane acknowledgement and
//! process reaping as separate facts.  A boundary is the POSIX session created
//! for one pane process; it can be confirmed reaped only after that session is
//! observed empty and no descendant observed by this process has escaped it.
//!
//! A PID is only a stable identity while its process lives, so every retained
//! PID is fenced by a birth token (its platform process start time).  A PID
//! whose token changed is a different process: it can neither prove anything
//! about this boundary nor be signalled by it.

use std::collections::BTreeMap;
use std::io;
use std::process::Command;
use std::time::Duration;

use thiserror::Error;

/// Poll cadence used for the short process-table proof window.
pub const PROCESS_OBSERVATION_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Non-blockingly collects one already-exited child process.
///
/// The caller must first establish that `pid` is its direct child and is no
/// longer owned by a live rmux pane. This function never signals or waits for a
/// running process.
pub fn try_reap_exited_child(pid: i32) -> Result<bool, ProcessBoundaryError> {
    if pid <= 0 {
        return Err(ProcessBoundaryError(format!(
            "child process pid {pid} is invalid"
        )));
    }
    let mut status = 0;
    #[allow(unsafe_code)]
    // SAFETY: waitpid receives a positive PID by value and a valid status
    // pointer. WNOHANG returns immediately for a still-running child.
    let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    if result == pid {
        Ok(true)
    } else if result == 0 {
        Ok(false)
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ECHILD) {
            // Another rmux lifecycle path already collected it.
            Ok(true)
        } else {
            Err(observation_error(
                &format!("waitpid({pid}, WNOHANG) failed"),
                error,
            ))
        }
    }
}

/// A process-table observation failure.  Callers must fail closed on this
/// error; absence could not be positively established.
#[derive(Debug, Error)]
#[error("{0}")]
pub struct ProcessBoundaryError(String);

/// Result of one observation of an owned POSIX session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessBoundaryObservation {
    members: Vec<TrackedProcess>,
    escaped_descendant_observed: bool,
}

impl ProcessBoundaryObservation {
    /// Whether at least one process still belongs to the POSIX session.
    #[must_use]
    pub fn member_present(&self) -> bool {
        !self.members.is_empty()
    }

    /// Whether a descendant observed in this process tree changed to another
    /// POSIX session.  Such a process invalidates positive reap proof.
    #[must_use]
    pub const fn escaped_descendant_observed(&self) -> bool {
        self.escaped_descendant_observed
    }
}

/// A conservative tracker for one rmux-created POSIX process session.
///
/// The leader PID is accepted only while it is itself a session leader.  PIDs
/// observed in the session or below an already tracked process are retained,
/// each with the birth token it was observed under, so that a later
/// reparent/session escape cannot be forgotten between polls while a recycled
/// PID can never be mistaken for the process that was retained under it.
#[derive(Clone, Debug)]
pub struct OwnedProcessBoundary {
    leader_pid: i32,
    leader_identity: Option<ProcessStartIdentity>,
    session_id: i32,
    tracked_pids: BTreeMap<i32, Option<ProcessStartIdentity>>,
    escaped_descendant_observed: bool,
}

impl OwnedProcessBoundary {
    /// Captures a pane PID only if it is currently an isolated POSIX session
    /// leader.  `None` means the process disappeared before it could be
    /// captured; every other mismatch is an error.
    pub fn capture(leader_pid: u32) -> Result<Option<Self>, ProcessBoundaryError> {
        let leader_pid = i32::try_from(leader_pid).map_err(|_| {
            ProcessBoundaryError(format!("process pid {leader_pid} is out of range"))
        })?;
        // The birth token is read before the session proof on purpose.  If the
        // PID were recycled between the two reads the retained token would be
        // the older process's, which can only fence this boundary off from the
        // new one; reading it afterwards could instead adopt an unrelated
        // process's token as proof of ownership.
        let leader_identity = process_start_identity(leader_pid);
        match process_session_id(leader_pid)? {
            Some(session_id) if session_id == leader_pid => Ok(Some(Self {
                leader_pid,
                leader_identity,
                session_id,
                tracked_pids: BTreeMap::from([(leader_pid, leader_identity)]),
                escaped_descendant_observed: false,
            })),
            Some(session_id) => Err(ProcessBoundaryError(format!(
                "pane pid {leader_pid} is not an isolated POSIX session leader (sid={session_id})"
            ))),
            None => Ok(None),
        }
    }

    /// Returns the numeric POSIX session identifier captured for diagnostics.
    #[must_use]
    pub const fn session_id(&self) -> i32 {
        self.session_id
    }

    /// Returns whether any observed descendant escaped the captured session.
    #[must_use]
    pub const fn escaped_descendant_observed(&self) -> bool {
        self.escaped_descendant_observed
    }

    /// Takes one process-table snapshot and advances the retained descendant
    /// set.  Races that make a row disappear are handled as absence, while an
    /// inability to inspect the table is an error.
    pub async fn observe(&mut self) -> Result<ProcessBoundaryObservation, ProcessBoundaryError> {
        let session_id = self.session_id;
        let leader = self.leader();
        let tracked_pids = self.tracked_pids.clone();
        let snapshot = tokio::task::spawn_blocking(move || {
            inspect_process_boundary_sync(session_id, leader, tracked_pids)
        })
        .await
        .map_err(|error| {
            ProcessBoundaryError(format!("process-table observer task failed: {error}"))
        })??;

        // The snapshot both extends the retained set and drops the PIDs it just
        // proved were recycled, so it is authoritative for the next poll.
        self.tracked_pids = snapshot.tracked_pids;
        self.escaped_descendant_observed |= snapshot.observation.escaped_descendant_observed;
        Ok(ProcessBoundaryObservation {
            members: snapshot.observation.members,
            escaped_descendant_observed: self.escaped_descendant_observed,
        })
    }

    const fn leader(&self) -> TrackedProcess {
        TrackedProcess {
            pid: self.leader_pid,
            identity: self.leader_identity,
        }
    }

    /// Waits until the session is positively observed empty or `timeout`
    /// expires.  `false` is also returned after any observed escape.
    pub async fn wait_until_reaped(
        &mut self,
        timeout: Duration,
    ) -> Result<bool, ProcessBoundaryError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let observation = self.observe().await?;
            if !observation.member_present() {
                return Ok(!observation.escaped_descendant_observed());
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(PROCESS_OBSERVATION_POLL_INTERVAL.min(deadline - now)).await;
        }
    }

    /// Force-terminates processes that still prove membership in the captured
    /// session and then observes that session until empty.
    ///
    /// A live POSIX session cannot be joined by an unrelated process, so exact
    /// `getsid(pid) == captured_sid` membership, re-proved against the member's
    /// birth token, is a safe local fallback when the rmux control plane is
    /// unavailable or its asynchronous teardown left descendants behind.
    /// Escaped descendants are deliberately not signalled here because a PID
    /// alone is not a sufficient stable identity after they leave that
    /// boundary.
    pub async fn force_reap(&mut self, timeout: Duration) -> Result<bool, ProcessBoundaryError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let observation = self.observe().await?;
            if !observation.member_present() {
                return Ok(!observation.escaped_descendant_observed());
            }
            for member in observation.members {
                signal_verified_session_member(member, self.session_id)?;
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(PROCESS_OBSERVATION_POLL_INTERVAL.min(deadline - now)).await;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessRecord {
    pid: i32,
    ppid: i32,
}

/// A PID together with the birth token it was observed under.
///
/// Two observations of one PID carrying different tokens are necessarily
/// different processes, because a PID can only be reused after the process
/// that held it was reaped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TrackedProcess {
    pid: i32,
    identity: Option<ProcessStartIdentity>,
}

/// One process-table row enriched with the two facts observed for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessRow {
    record: ProcessRecord,
    session_id: Option<i32>,
    start_identity: Option<ProcessStartIdentity>,
}

struct InspectionSnapshot {
    observation: ProcessBoundaryObservation,
    tracked_pids: BTreeMap<i32, Option<ProcessStartIdentity>>,
}

fn inspect_process_boundary_sync(
    session_id: i32,
    leader: TrackedProcess,
    tracked_pids: BTreeMap<i32, Option<ProcessStartIdentity>>,
) -> Result<InspectionSnapshot, ProcessBoundaryError> {
    let rows = process_table()?
        .into_iter()
        .map(|record| {
            let row_session_id = process_session_id(record.pid)?;
            let start_identity = process_start_identity(record.pid);
            Ok(ProcessRow {
                record,
                session_id: row_session_id,
                start_identity,
            })
        })
        .collect::<Result<Vec<_>, ProcessBoundaryError>>()?;
    Ok(classify_process_snapshot(
        session_id,
        leader,
        tracked_pids,
        &rows,
    ))
}

fn classify_process_snapshot(
    session_id: i32,
    leader: TrackedProcess,
    mut tracked_pids: BTreeMap<i32, Option<ProcessStartIdentity>>,
    rows: &[ProcessRow],
) -> InspectionSnapshot {
    // Fence PID reuse first: a retained PID whose birth token changed is a
    // different process, so it must stop seeding descendant discovery, stop
    // latching the escape flag, and stop being selectable for termination.
    for row in rows {
        if let Some(&recorded) = tracked_pids.get(&row.record.pid)
            && is_recycled(recorded, row.start_identity)
        {
            tracked_pids.remove(&row.record.pid);
        }
    }

    // The captured session identifier is the leader PID, so a recycled leader
    // PID means that identifier now names some other session. A kernel only
    // releases a session leader's PID once its session is empty, so this is
    // positive evidence of this boundary's absence, never of membership in it.
    let session_id_recycled = rows.iter().any(|row| {
        row.record.pid == leader.pid && is_recycled(leader.identity, row.start_identity)
    });

    loop {
        let before = tracked_pids.len();
        for row in rows {
            if tracked_pids.contains_key(&row.record.ppid) {
                tracked_pids
                    .entry(row.record.pid)
                    .or_insert(row.start_identity);
            }
        }
        if tracked_pids.len() == before {
            break;
        }
    }

    let mut members = Vec::new();
    let mut escaped_descendant_observed = false;
    for row in rows {
        let observed = TrackedProcess {
            pid: row.record.pid,
            identity: row.start_identity,
        };
        let Some(current_session_id) = row.session_id else {
            // A zombie can remain visible in `ps` after `getsid` has already
            // started returning ESRCH (observed on macOS). If this exact PID
            // was previously proven to belong to the boundary, its process
            // table row is still positive evidence that reaping is incomplete.
            // Never turn an uninspectable tracked row into an absence claim.
            if tracked_pids.contains_key(&row.record.pid) {
                members.push(observed);
            }
            continue;
        };
        if current_session_id == session_id && !session_id_recycled {
            members.push(observed);
            // A PID reused inside this still-live session is a real member, so
            // membership also refreshes the token retained for it.
            tracked_pids
                .entry(row.record.pid)
                .or_insert(row.start_identity);
        } else if tracked_pids.contains_key(&row.record.pid) && row.record.pid != leader.pid {
            escaped_descendant_observed = true;
        }
    }
    members.sort_unstable_by_key(|member| member.pid);
    members.dedup_by_key(|member| member.pid);
    InspectionSnapshot {
        observation: ProcessBoundaryObservation {
            members,
            escaped_descendant_observed,
        },
        tracked_pids,
    }
}

/// Whether `observed` proves that a PID now names a different process than the
/// one `recorded` was taken from.
///
/// An unreadable token on either side is an unknown, never a proof of reuse:
/// the caller must keep treating the PID as the process it retained.
fn is_recycled(
    recorded: Option<ProcessStartIdentity>,
    observed: Option<ProcessStartIdentity>,
) -> bool {
    matches!((recorded, observed), (Some(recorded), Some(observed)) if recorded != observed)
}

fn process_table() -> Result<Vec<ProcessRecord>, ProcessBoundaryError> {
    let output = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid="])
        .output()
        .map_err(|error| observation_error("could not run /bin/ps", error))?;
    if !output.status.success() {
        return Err(ProcessBoundaryError(format!(
            "/bin/ps exited with {}",
            output.status
        )));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|error| {
        ProcessBoundaryError(format!("/bin/ps emitted non-UTF-8 output: {error}"))
    })?;
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_process_record)
        .collect()
}

fn parse_process_record(line: &str) -> Result<ProcessRecord, ProcessBoundaryError> {
    let mut fields = line.split_whitespace();
    let pid = fields.next().and_then(|value| value.parse::<i32>().ok());
    let ppid = fields.next().and_then(|value| value.parse::<i32>().ok());
    if fields.next().is_some() || pid.is_none_or(|pid| pid <= 0) || ppid.is_none_or(|ppid| ppid < 0)
    {
        return Err(ProcessBoundaryError(format!(
            "unrecognized /bin/ps process row: {line:?}"
        )));
    }
    Ok(ProcessRecord {
        pid: pid.expect("positive pid was checked"),
        ppid: ppid.expect("non-negative ppid was checked"),
    })
}

fn signal_verified_session_member(
    member: TrackedProcess,
    expected_session_id: i32,
) -> Result<(), ProcessBoundaryError> {
    if !member_identity_still_proven(member, expected_session_id)? {
        return Ok(());
    }
    let pid = member.pid;
    #[allow(unsafe_code)]
    // SAFETY: `kill` receives a positive PID just obtained from a process-table
    // snapshot and revalidated against the still-live isolated POSIX session
    // and against the exact process observed under that PID.
    let result = unsafe { libc::kill(pid, libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(observation_error(
            &format!("failed to terminate verified session member {pid}"),
            error,
        ))
    }
}

/// Re-proves, immediately before any signal, that the PID still names the exact
/// process that was observed inside the captured POSIX session.
fn member_identity_still_proven(
    member: TrackedProcess,
    expected_session_id: i32,
) -> Result<bool, ProcessBoundaryError> {
    if process_session_id(member.pid)? != Some(expected_session_id) {
        return Ok(false);
    }
    // A member observed one snapshot ago can have exited and had its PID handed
    // to an unrelated process since. A changed birth token proves exactly that,
    // and this boundary must never signal a process it did not create.
    Ok(!is_recycled(
        member.identity,
        process_start_identity(member.pid),
    ))
}

#[allow(unsafe_code)]
fn process_session_id(pid: i32) -> Result<Option<i32>, ProcessBoundaryError> {
    // SAFETY: `getsid` accepts a process identifier by value and does not
    // dereference caller memory. Positive identifiers came from rmux or ps.
    let session_id = unsafe { libc::getsid(pid) };
    if session_id >= 0 {
        return Ok(Some(session_id));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(None)
    } else {
        Err(observation_error(&format!("getsid({pid}) failed"), error))
    }
}

fn observation_error(context: &str, error: io::Error) -> ProcessBoundaryError {
    ProcessBoundaryError(format!("{context}: {error}"))
}

/// An opaque platform birth token for one process.
///
/// The value itself is meaningless; only equality between two observations of
/// the same PID is used, and a process start time cannot survive PID reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessStartIdentity {
    /// Coarse start counter: `starttime` ticks on Linux, seconds on macOS.
    coarse: u64,
    /// Fine start counter: always `0` on Linux, microseconds on macOS.
    fine: u64,
}

/// Reads the birth token of `pid`.
///
/// `None` means the token could not be established at all — the process is
/// already gone, it is not inspectable, or the platform exposes no token. It is
/// never a claim that the PID belongs to a different process: every caller
/// must fail closed and keep treating such a PID as the one it retained.
#[cfg(target_os = "macos")]
fn process_start_identity(pid: i32) -> Option<ProcessStartIdentity> {
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    #[allow(unsafe_code)]
    // SAFETY: `info` is writable storage of exactly the requested flavor size,
    // `proc_pidinfo` writes at most that many bytes into it, and `pid` is
    // passed by value.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read < size {
        return None;
    }
    #[allow(unsafe_code)]
    // SAFETY: `proc_pidinfo` reported a complete `proc_bsdinfo` structure.
    let info = unsafe { info.assume_init() };
    Some(ProcessStartIdentity {
        coarse: info.pbi_start_tvsec,
        fine: info.pbi_start_tvusec,
    })
}

/// Reads the birth token of `pid`.  See the macOS implementation for the
/// contract; `None` is always an unknown and never a proof of PID reuse.
#[cfg(target_os = "linux")]
fn process_start_identity(pid: i32) -> Option<ProcessStartIdentity> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The `comm` field is parenthesised and may itself contain spaces, so the
    // positional fields only start after its closing parenthesis. Index 19 of
    // that remainder is `starttime` (field 22 of proc(5)).
    let (_, fields) = stat.rsplit_once(") ")?;
    let start_ticks = fields.split_whitespace().nth(19)?.parse::<u64>().ok()?;
    Some(ProcessStartIdentity {
        coarse: start_ticks,
        fine: 0,
    })
}

/// Reads the birth token of `pid`.  Platforms without a supported source
/// expose no token, which keeps the pre-existing conservative behavior.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_identity(_pid: i32) -> Option<ProcessStartIdentity> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn identity(value: u64) -> Option<ProcessStartIdentity> {
        Some(ProcessStartIdentity {
            coarse: value,
            fine: 0,
        })
    }

    const fn leader(pid: i32, start_identity: Option<ProcessStartIdentity>) -> TrackedProcess {
        TrackedProcess {
            pid,
            identity: start_identity,
        }
    }

    const fn row(
        pid: i32,
        ppid: i32,
        session_id: Option<i32>,
        start_identity: Option<ProcessStartIdentity>,
    ) -> ProcessRow {
        ProcessRow {
            record: ProcessRecord { pid, ppid },
            session_id,
            start_identity,
        }
    }

    fn member_pids(observation: &ProcessBoundaryObservation) -> Vec<i32> {
        observation
            .members
            .iter()
            .map(|member| member.pid)
            .collect()
    }

    #[test]
    fn process_rows_are_parsed_strictly() {
        assert_eq!(
            parse_process_record("  123   42").unwrap(),
            ProcessRecord { pid: 123, ppid: 42 }
        );
        assert!(parse_process_record("123").is_err());
        assert!(parse_process_record("123 42 extra").is_err());
        assert!(parse_process_record("0 42").is_err());
        assert!(parse_process_record("123 -1").is_err());
    }

    #[test]
    fn observed_descendant_escape_is_sticky_across_reparenting() {
        let first = classify_process_snapshot(
            10,
            leader(10, identity(1)),
            BTreeMap::from([(10, identity(1))]),
            &[
                row(10, 1, Some(10), identity(1)),
                row(11, 10, Some(11), identity(2)),
            ],
        );
        assert!(first.observation.member_present());
        assert!(first.observation.escaped_descendant_observed());
        assert!(first.tracked_pids.contains_key(&11));

        let second = classify_process_snapshot(
            10,
            leader(10, identity(1)),
            first.tracked_pids,
            &[row(11, 1, Some(11), identity(2))],
        );
        assert!(!second.observation.member_present());
        assert!(second.observation.escaped_descendant_observed());
    }

    #[test]
    fn tracked_process_with_no_session_id_remains_present_until_row_disappears() {
        let zombie_like = classify_process_snapshot(
            10,
            leader(10, identity(1)),
            BTreeMap::from([(10, identity(1))]),
            &[row(10, 1, None, identity(1))],
        );
        assert_eq!(member_pids(&zombie_like.observation), vec![10]);
        assert!(zombie_like.observation.member_present());

        let absent =
            classify_process_snapshot(10, leader(10, identity(1)), zombie_like.tracked_pids, &[]);
        assert!(!absent.observation.member_present());
    }

    /// A recycled descendant PID must never latch the sticky escape flag: that
    /// flag is what keeps `close()` unconfirmable, and an unrelated process
    /// inheriting the number would keep it latched forever.
    #[test]
    fn a_recycled_tracked_pid_does_not_latch_the_escape_flag() {
        let snapshot = classify_process_snapshot(
            10,
            leader(10, identity(1)),
            BTreeMap::from([(10, identity(1)), (11, identity(2))]),
            &[
                row(10, 1, Some(10), identity(1)),
                // pid 11 was reaped and the number handed to a stranger.
                row(11, 1, Some(11), identity(7)),
            ],
        );
        assert!(!snapshot.observation.escaped_descendant_observed());
        assert_eq!(member_pids(&snapshot.observation), vec![10]);
        assert!(!snapshot.tracked_pids.contains_key(&11));
    }

    /// The stranger's own children must not be adopted through the retained
    /// PID by the transitive ppid fixpoint.
    #[test]
    fn descendants_of_a_recycled_tracked_pid_are_not_adopted() {
        let snapshot = classify_process_snapshot(
            10,
            leader(10, identity(1)),
            BTreeMap::from([(10, identity(1)), (11, identity(2))]),
            &[
                row(10, 1, Some(10), identity(1)),
                row(11, 1, Some(11), identity(7)),
                row(12, 11, Some(11), identity(8)),
            ],
        );
        assert!(!snapshot.observation.escaped_descendant_observed());
        assert_eq!(member_pids(&snapshot.observation), vec![10]);
        assert!(!snapshot.tracked_pids.contains_key(&12));
    }

    /// Once the leader PID names a different process, the captured session
    /// identifier names some other session. Nothing in it may be reported as a
    /// member, because members are exactly the set `force_reap` signals.
    #[test]
    fn a_recycled_session_leader_is_never_a_member() {
        let snapshot = classify_process_snapshot(
            10,
            leader(10, identity(1)),
            BTreeMap::from([(10, identity(1))]),
            &[
                row(10, 1, Some(10), identity(9)),
                row(12, 10, Some(10), identity(10)),
            ],
        );
        assert!(member_pids(&snapshot.observation).is_empty());
        assert!(!snapshot.observation.escaped_descendant_observed());
        assert!(!snapshot.tracked_pids.contains_key(&10));
        assert!(!snapshot.tracked_pids.contains_key(&12));
    }

    /// A PID reused *inside* the still-live captured session is a genuine
    /// member: the reuse fence must not invent an absence.
    #[test]
    fn a_pid_reused_inside_the_captured_session_is_still_a_member() {
        let snapshot = classify_process_snapshot(
            10,
            leader(10, identity(1)),
            BTreeMap::from([(10, identity(1)), (11, identity(2))]),
            &[
                row(10, 1, Some(10), identity(1)),
                row(11, 10, Some(10), identity(7)),
            ],
        );
        assert_eq!(member_pids(&snapshot.observation), vec![10, 11]);
        assert!(!snapshot.observation.escaped_descendant_observed());
        assert_eq!(snapshot.tracked_pids.get(&11), Some(&identity(7)));
    }

    /// Fail closed: an unreadable birth token is an unknown, so the escape
    /// proof keeps behaving exactly as it did before tokens existed.
    #[test]
    fn unreadable_birth_tokens_keep_the_conservative_escape_proof() {
        for (recorded, observed) in [(identity(2), None), (None, identity(2)), (None, None)] {
            let snapshot = classify_process_snapshot(
                10,
                leader(10, identity(1)),
                BTreeMap::from([(10, identity(1)), (11, recorded)]),
                &[
                    row(10, 1, Some(10), identity(1)),
                    row(11, 1, Some(11), observed),
                ],
            );
            assert!(
                snapshot.observation.escaped_descendant_observed(),
                "unknown token pair ({recorded:?}, {observed:?}) must not clear the escape proof"
            );
            assert!(snapshot.tracked_pids.contains_key(&11));
        }
    }

    /// Fail closed: an unreadable leader token leaves `getsid` membership as
    /// the sole, pre-existing source of truth.
    #[test]
    fn an_unreadable_leader_birth_token_keeps_membership_evidence() {
        let snapshot = classify_process_snapshot(
            10,
            leader(10, None),
            BTreeMap::from([(10, None)]),
            &[row(10, 1, Some(10), identity(5))],
        );
        assert_eq!(member_pids(&snapshot.observation), vec![10]);
    }

    #[test]
    fn birth_tokens_prove_reuse_only_when_both_observations_are_known() {
        assert!(is_recycled(identity(1), identity(2)));
        assert!(!is_recycled(identity(1), identity(1)));
        assert!(!is_recycled(identity(1), None));
        assert!(!is_recycled(None, identity(1)));
        assert!(!is_recycled(None, None));
    }

    /// Exercises the real process table, the real `getsid`, and the real birth
    /// token reader together.  The reuse fence assumes a kernel only releases a
    /// session leader's PID once its session is empty; this process is in its
    /// own session, so a false "recycled leader" here would hide it as an
    /// unexplained absence instead.
    #[test]
    fn a_live_process_table_snapshot_still_proves_this_process_is_a_member() {
        let pid = i32::try_from(std::process::id()).expect("test pid fits pid_t");
        let session_id = process_session_id(pid)
            .expect("getsid on self succeeds")
            .expect("the test process is alive");
        let leader_identity = process_start_identity(session_id);
        let snapshot = inspect_process_boundary_sync(
            session_id,
            leader(session_id, leader_identity),
            BTreeMap::from([(session_id, leader_identity)]),
        )
        .expect("the live process table is observable");
        assert!(
            member_pids(&snapshot.observation).contains(&pid),
            "this process must be observed inside its own POSIX session"
        );
    }

    /// The live re-verify performed immediately before every `SIGKILL`.
    #[test]
    fn a_member_whose_birth_token_changed_is_never_signalled() {
        let pid = i32::try_from(std::process::id()).expect("test pid fits pid_t");
        let session_id = process_session_id(pid)
            .expect("getsid on self succeeds")
            .expect("the test process is alive");
        let observed = process_start_identity(pid);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        assert!(
            observed.is_some(),
            "supported platforms must expose a birth token"
        );
        assert_eq!(
            process_start_identity(pid),
            observed,
            "a live process keeps one birth token"
        );

        assert!(
            member_identity_still_proven(
                TrackedProcess {
                    pid,
                    identity: observed
                },
                session_id
            )
            .unwrap()
        );
        assert!(
            !member_identity_still_proven(
                TrackedProcess {
                    pid,
                    identity: observed
                },
                session_id + 1
            )
            .unwrap(),
            "a PID outside the captured session is never signalled"
        );
        if observed.is_some() {
            assert!(
                !member_identity_still_proven(
                    TrackedProcess {
                        pid,
                        identity: identity(u64::MAX)
                    },
                    session_id
                )
                .unwrap(),
                "a recycled PID is never signalled even inside the captured session"
            );
        }
    }
}
