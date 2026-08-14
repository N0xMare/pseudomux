//! The parent that kills, and then interrogates the store the way a recovering
//! daemon would.
//!
//! ```text
//! crash-harness update  <trials>   <child-binary> [work-root]
//! crash-harness create  <trials>   <child-binary> [work-root]
//! crash-harness restart <restarts> <child-binary> [work-root]
//! ```
//!
//! Exit 0 when every trial held, 1 when any did not. Every mode removes its own
//! work root on the way out; each root lives under `TMPDIR` and carries this
//! process's pid, so a killed harness leaves one nameable directory and never a
//! `/tmp` prefix the Gate A residue audit scans for.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use pmux_crash_harness::{Jitter, NOW, kill9, spec};
use pseudomux_protocol::v1::{AgentId, AgentVersion};
use pseudomux_service::agent::AgentStore;
use sha2::{Digest, Sha256};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");
    let rounds: u64 = args
        .get(2)
        .map_or(0, |value| value.parse().expect("rounds"));
    let child_binary = PathBuf::from(args.get(3).expect("child binary"));
    let work_root = args
        .get(4)
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join(format!("pmux-crash-{mode}-{}", std::process::id()));
    std::fs::create_dir_all(&work_root).expect("work root");

    let failures = match mode {
        "update" => update_mode(rounds, &child_binary, &work_root),
        "create" => create_mode(rounds, &child_binary, &work_root),
        "restart" => restart_mode(rounds, &child_binary, &work_root),
        other => {
            eprintln!("unknown mode {other:?}; expected `update`, `create` or `restart`");
            std::process::exit(2);
        }
    };
    let _ = std::fs::remove_dir_all(&work_root);
    if failures > 0 {
        std::process::exit(1);
    }
}

/// Spawns the child and blocks until it reports its first completed operation,
/// so the kill lands in a steady-state loop and never in process startup.
fn spawn_running(
    child_binary: &Path,
    args: &[String],
) -> (std::process::Child, mpsc::Receiver<String>) {
    let mut child = Command::new(child_binary)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let stdout = child.stdout.take().expect("stdout");
    let (sender, receiver) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                return;
            }
        }
    });
    let first = receiver
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the child never completed one operation");
    assert!(!first.starts_with("err "), "the child was refused: {first}");
    (child, receiver)
}

/// How long one full update cycle takes on THIS host, so the kill offset can be
/// spread uniformly across one. Measured rather than assumed: it is 20ms here
/// and would be a different number on any other filesystem.
fn calibrate(child_binary: &Path, work_root: &Path) -> u64 {
    let root = work_root.join("calibration").join("agents");
    let store = AgentStore::open(&root).expect("open");
    let created = store.create(spec(0), NOW).expect("create");
    let mut child = Command::new(child_binary)
        .args([
            "update".to_owned(),
            root.display().to_string(),
            created.agent_id.hyphenated().to_string(),
            "1".to_owned(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let stdout = child.stdout.take().expect("stdout");
    let mut lines = BufReader::new(stdout).lines();
    lines.next();
    let started = std::time::Instant::now();
    let rounds = 200u64;
    for _ in 0..rounds {
        lines.next();
    }
    let elapsed = started.elapsed();
    kill9(&child);
    let _ = child.wait();
    u64::try_from(elapsed.as_micros()).unwrap_or(1_000) / rounds
}

fn published_max(agent_dir: &Path) -> u64 {
    let mut max = 0;
    for entry in std::fs::read_dir(agent_dir).expect("read dir").flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(number) = name.strip_suffix(".json")
            && let Ok(version) = number.parse::<u64>()
        {
            max = max.max(version);
        }
    }
    max
}

/// DEFECT TEN. Kill a looping updater at a jittered offset, then check the four
/// things the store owes a caller afterwards.
fn update_mode(trials: u64, child_binary: &Path, work_root: &Path) -> u64 {
    let cycle_us = calibrate(child_binary, work_root);
    println!("--- calibration: one update cycle is ~{cycle_us}us on this host");
    let window_us = (cycle_us * 6).max(1_000);
    let mut jitter = Jitter::from_clock();
    let (mut broken, mut in_window, mut temp_residue) = (0u64, 0u64, 0u64);

    for trial in 0..trials {
        let root = work_root.join(format!("trial-{trial}")).join("agents");
        let store = AgentStore::open(&root).expect("open");
        let agent_id = store.create(spec(0), NOW).expect("create").agent_id;
        let (mut child, receiver) = spawn_running(
            child_binary,
            &[
                "update".to_owned(),
                root.display().to_string(),
                agent_id.hyphenated().to_string(),
                "1".to_owned(),
            ],
        );
        std::thread::sleep(std::time::Duration::from_micros(jitter.next(window_us)));
        kill9(&child);
        let _ = child.wait();
        let mut last_ok = 1u64;
        while let Ok(line) = receiver.recv_timeout(std::time::Duration::from_millis(50)) {
            if let Some(rest) = line.strip_prefix("ok ")
                && let Ok(version) = rest.trim().parse::<u64>()
            {
                last_ok = version;
            }
        }

        let agent_dir = root.join(agent_id.hyphenated().to_string());
        let head_on_disk: u64 = std::fs::read_to_string(agent_dir.join("head"))
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(0);
        let newest = published_max(&agent_dir);
        temp_residue += std::fs::read_dir(&agent_dir)
            .expect("read dir")
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .count() as u64;

        // Read BEFORE any repairing write: what a recovering reader sees.
        let read_head = store.get(agent_id, None).map_or_else(
            |error| format!("Err({})", error.message),
            |d| d.version.to_string(),
        );
        let listed = store.list().expect("list");
        let listed_version = listed
            .agents
            .iter()
            .find(|agent| agent.agent_id == agent_id)
            .map_or_else(|| "absent".to_owned(), |agent| agent.version.to_string());

        let retry = |version: u64| -> String {
            AgentVersion::new(version).map_or_else(
                |_| "no version 0".to_owned(),
                |fence| match store.update(agent_id, fence, spec(version), NOW + 99) {
                    Ok(descriptor) => format!("Ok(v{})", descriptor.version),
                    Err(error) => format!("{:?}: {}", error.code, error.message),
                },
            )
        };
        let first_retry = retry(head_on_disk);
        let second_retry = retry(head_on_disk.max(newest));

        let mut reasons = Vec::new();
        if read_head != newest.to_string() {
            reasons.push(format!(
                "get(None) answered {read_head}, not the newest published {newest}"
            ));
        }
        if listed_version != newest.to_string() {
            reasons.push(format!(
                "list reported {listed_version}, not the newest published {newest}"
            ));
        }
        if !listed.unreadable.is_empty() {
            reasons.push(format!(
                "list reported {} unreadable",
                listed.unreadable.len()
            ));
        }
        if !first_retry.starts_with("Ok(") && !second_retry.starts_with("Ok(") {
            reasons.push("no fence a caller can reach is accepted: WEDGED".to_owned());
        }
        if first_retry.contains(&format!(
            "is at version {newest}, not the expected version {head_on_disk}"
        )) && second_retry.contains(&format!(
            "is at version {head_on_disk}, not the expected version {newest}"
        )) {
            reasons.push(
                "consecutive attempts were told the fence is stale in BOTH directions".to_owned(),
            );
        }

        if newest > head_on_disk {
            in_window += 1;
        }
        if reasons.is_empty() {
            println!(
                "trial {trial}: OK head={head_on_disk} published_max={newest} last_ok={last_ok} \
                 get(None)=v{read_head} list=v{listed_version} retry@{head_on_disk} -> {first_retry}"
            );
        } else {
            broken += 1;
            println!(
                "trial {trial}: BROKEN head={head_on_disk} published_max={newest} last_ok={last_ok}"
            );
            println!("  get(None) -> {read_head}   list -> {listed_version}");
            println!("  retry@{head_on_disk} -> {first_retry}");
            println!("  retry@{} -> {second_retry}", head_on_disk.max(newest));
            for reason in &reasons {
                println!("  ! {reason}");
            }
        }
    }
    println!(
        "--- {trials} trials, {broken} broken, {in_window} landed in the window \
         (published_max > head), {temp_residue} orphan .tmp files left behind"
    );
    broken
}

/// `create_agent` is assembled under a staging name and published in one
/// `rename(2)`, so a listing can never see a half-made agent.
fn create_mode(trials: u64, child_binary: &Path, work_root: &Path) -> u64 {
    let mut jitter = Jitter::from_clock();
    let (mut broken, mut staging_residue) = (0u64, 0u64);
    for trial in 0..trials {
        let root = work_root.join(format!("trial-{trial}")).join("agents");
        let store = AgentStore::open(&root).expect("open");
        let (mut child, receiver) = spawn_running(
            child_binary,
            &["create".to_owned(), root.display().to_string()],
        );
        std::thread::sleep(std::time::Duration::from_micros(jitter.next(60_000)));
        kill9(&child);
        let _ = child.wait();
        while receiver
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_ok()
        {}

        let listed = store.list().expect("list");
        let mut reasons = Vec::new();
        if !listed.unreadable.is_empty() {
            reasons.push(format!("list reported unreadable: {:?}", listed.unreadable));
        }
        for summary in &listed.agents {
            if let Err(error) = store.get(summary.agent_id, None) {
                reasons.push(format!(
                    "get({}) refused: {}",
                    summary.agent_id, error.message
                ));
            }
        }
        let staging = std::fs::read_dir(&root)
            .expect("read root")
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(".pending-"))
            .count() as u64;
        staging_residue += staging;

        if reasons.is_empty() {
            println!(
                "trial {trial}: OK listed={} staging_residue={staging}",
                listed.agents.len()
            );
        } else {
            broken += 1;
            println!("trial {trial}: BROKEN listed={}", listed.agents.len());
            for reason in &reasons {
                println!("  ! {reason}");
            }
        }
    }
    println!(
        "--- {trials} trials, {broken} broken, {staging_residue} orphan .pending- directories \
         left behind"
    );
    broken
}

/// The two claims one crash cannot test: a published version is never
/// observable partial, and `(agent_id, version)` denotes ONE byte-string for all
/// time. One store, N crash-and-restart cycles, every version re-read and
/// re-digested after every kill.
fn restart_mode(restarts: u64, child_binary: &Path, work_root: &Path) -> u64 {
    let root = work_root.join("agents");
    let store = AgentStore::open(&root).expect("open");
    let agent_id: AgentId = store.create(spec(0), NOW).expect("create").agent_id;
    let agent_dir = root.join(agent_id.hyphenated().to_string());
    let mut recorded: Vec<Option<String>> = vec![None];
    let mut jitter = Jitter::from_clock();
    let (mut broken, mut mutated, mut unreadable) = (0u64, 0u64, 0u64);

    for cycle in 0..restarts {
        // Each cycle starts where a recovering daemon would: at the resolved
        // head, not at a remembered one.
        let start = match store.get(agent_id, None) {
            Ok(descriptor) => descriptor.version.get(),
            Err(error) => {
                println!("cycle {cycle}: BROKEN get(None) refused: {}", error.message);
                broken += 1;
                break;
            }
        };
        let mut child = Command::new(child_binary)
            .args([
                "update".to_owned(),
                root.display().to_string(),
                agent_id.hyphenated().to_string(),
                start.to_string(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let stdout = child.stdout.take().expect("stdout");
        let (sender, receiver) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        let first = receiver
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap_or_default();
        if first.starts_with("err ") {
            println!("cycle {cycle}: BROKEN child refused at fence {start}: {first}");
            broken += 1;
            kill9(&child);
            let _ = child.wait();
            break;
        }
        std::thread::sleep(std::time::Duration::from_micros(jitter.next(120_000)));
        kill9(&child);
        let _ = child.wait();

        for value in 1..=published_max(&agent_dir) {
            let version = AgentVersion::new(value).expect("version");
            if let Err(error) = store.get(agent_id, Some(version)) {
                unreadable += 1;
                println!("cycle {cycle}: v{value} UNREADABLE: {}", error.message);
            }
            let digest = std::fs::read(agent_dir.join(format!("{value}.json")))
                .map(|bytes| format!("{:x}", Sha256::digest(&bytes)))
                .unwrap_or_else(|error| format!("<unreadable: {error}>"));
            if recorded.len() <= value as usize {
                recorded.resize(value as usize + 1, None);
            }
            match &recorded[value as usize] {
                Some(previous) if *previous != digest => {
                    mutated += 1;
                    println!("cycle {cycle}: v{value} CHANGED: was {previous}, now {digest}");
                }
                Some(_) => {}
                None => recorded[value as usize] = Some(digest),
            }
        }
        let listed = store.list().expect("list");
        if !listed.unreadable.is_empty() {
            broken += 1;
            println!(
                "cycle {cycle}: BROKEN list reported {:?}",
                listed.unreadable
            );
        }
    }
    println!(
        "--- {restarts} crash-restart cycles on one store, {} versions published, {broken} broken, \
         {mutated} versions whose bytes changed, {unreadable} versions that stopped reading",
        published_max(&agent_dir)
    );
    broken + mutated + unreadable
}
