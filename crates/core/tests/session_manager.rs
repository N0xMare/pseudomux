use pseudomux_core::input::{KeyEvent, KittyKeyboardMode};
use pseudomux_core::session::StartSpec;
use pseudomux_core::session::manager::SessionManager;
use pseudomux_core::session::state::{LoggingMode, ScrollbackConfig, TerminalSize};

fn test_spec() -> StartSpec {
    StartSpec {
        profile: None,
        agent: "test".into(),
        program: "sh".into(),
        args: vec!["-c".into(), "cat".into()],
        env: vec![],
        cwd: None,
        size: TerminalSize { rows: 24, cols: 80 },
        scrollback: ScrollbackConfig {
            raw_bytes: 65536,
            stripped_bytes: 65536,
        },
        logging: LoggingMode::Metadata,
        log_dir_base: None,
        agent_kind: None,
        capability_policy_keyboard: None,
        input_profile_name: None,
        env_remove: vec![],
        adapter: None,
        record_path: None,
        name: None,
    }
}

#[test]
fn test_send_key_enter() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let mgr = SessionManager::new();
    let handle = mgr.start_session(test_spec()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    mgr.send_key(handle, KeyEvent::Enter).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let (chunks, _) = mgr.read_since(handle, 0).unwrap();
    assert!(
        !chunks.is_empty(),
        "should have received output after sending Enter"
    );
    mgr.terminate(handle).ok();
}

#[test]
fn test_send_enter_uses_encoder() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let mgr = SessionManager::new();
    let handle = mgr.start_session(test_spec()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    mgr.send_enter(handle).unwrap();
    mgr.send_key(handle, KeyEvent::Enter).unwrap();
    mgr.terminate(handle).ok();
}

#[test]
fn test_terminal_state_default() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let mgr = SessionManager::new();
    let handle = mgr.start_session(test_spec()).unwrap();

    let state = mgr.terminal_state(handle).unwrap();
    assert_eq!(state.keyboard_mode, KittyKeyboardMode::Legacy);
    assert!(!state.bracketed_paste);
    mgr.terminate(handle).ok();
}

#[test]
fn test_send_text_plain() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _guard = rt.enter();

    let mgr = SessionManager::new();
    let handle = mgr.start_session(test_spec()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    mgr.send_text(handle, "hello").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(100));

    let (chunks, _) = mgr.read_since(handle, 0).unwrap();
    assert!(
        !chunks.is_empty(),
        "should have received output after sending text"
    );
    mgr.terminate(handle).ok();
}
