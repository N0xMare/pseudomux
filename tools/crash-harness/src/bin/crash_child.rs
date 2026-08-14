//! The process that gets killed.
//!
//! ```text
//! crash-child update <store-root> <agent-id> <start-version>
//! crash-child create <store-root>
//! ```
//!
//! One line per completed operation on stdout, flushed, so the parent can see
//! how far it got before the signal landed.

use std::io::Write;

use pmux_crash_harness::{NOW, spec};
use pseudomux_protocol::v1::{AgentId, AgentVersion};
use pseudomux_service::agent::AgentStore;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");
    let root = std::path::PathBuf::from(args.get(2).expect("store root"));
    let store = AgentStore::open(&root).expect("open");
    let mut stdout = std::io::stdout();

    match mode {
        "update" => {
            let agent_id: AgentId = args[3].parse().expect("agent id");
            let mut version: u64 = args[4].parse().expect("start version");
            loop {
                let expected = AgentVersion::new(version).expect("version");
                match store.update(agent_id, expected, spec(version), NOW + version) {
                    Ok(descriptor) => {
                        version = descriptor.version.get();
                        writeln!(stdout, "ok {version}").expect("write");
                        stdout.flush().expect("flush");
                    }
                    Err(error) => {
                        writeln!(stdout, "err {:?} {}", error.code, error.message).expect("write");
                        stdout.flush().expect("flush");
                        std::process::exit(3);
                    }
                }
            }
        }
        "create" => {
            let mut marker = 0u64;
            loop {
                marker += 1;
                match store.create(spec(marker), NOW + marker) {
                    Ok(descriptor) => {
                        writeln!(stdout, "ok {}", descriptor.agent_id).expect("write");
                        stdout.flush().expect("flush");
                    }
                    Err(error) => {
                        writeln!(stdout, "err {:?} {}", error.code, error.message).expect("write");
                        stdout.flush().expect("flush");
                        std::process::exit(3);
                    }
                }
            }
        }
        other => {
            eprintln!("unknown mode {other:?}; expected `update` or `create`");
            std::process::exit(2);
        }
    }
}
