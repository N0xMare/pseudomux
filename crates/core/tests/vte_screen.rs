use pseudomux_core::vte::{ScreenChange, ScreenModel, ScreenRegions};

#[test]
fn simple_text_produces_content_change() {
    let regions = ScreenRegions::opencode(24, 80);
    let mut model = ScreenModel::new(24, 80, 0, regions);
    let changes = model.process(b"Hello, world!");
    assert!(changes.iter().any(|c| matches!(c,
        ScreenChange::ContentRowChanged { row: 0, new, .. } if new == "Hello, world!"
    )));
}

#[test]
fn status_bar_isolation() {
    let regions = ScreenRegions::opencode(24, 80);
    let mut model = ScreenModel::new(24, 80, 0, regions);
    let esc = b"\x1b[24;1HStatus Line";
    let changes = model.process(esc);
    assert!(changes.iter().any(|c| matches!(c,
        ScreenChange::StatusBarChanged { new, .. } if new.contains("Status Line")
    )));
    assert!(
        !changes
            .iter()
            .any(|c| matches!(c, ScreenChange::ContentRowChanged { row: 22, .. }))
    );
    assert!(
        !changes
            .iter()
            .any(|c| matches!(c, ScreenChange::ContentRowChanged { row: 23, .. }))
    );
}

#[test]
fn screen_clear_detection() {
    let regions = ScreenRegions::opencode(24, 80);
    let mut model = ScreenModel::new(24, 80, 0, regions);
    model.process(b"Some content here");
    let changes = model.process(b"\x1b[2J\x1b[H");
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, ScreenChange::ScreenCleared))
    );
}

#[test]
fn content_text_extraction() {
    let regions = ScreenRegions::opencode(24, 80);
    let mut model = ScreenModel::new(24, 80, 0, regions);
    model.process(b"Line 1\r\nLine 2");
    let text = model.content_text();
    assert!(text.contains("Line 1"));
    assert!(text.contains("Line 2"));
}

#[test]
fn resize_resets_state() {
    let regions = ScreenRegions::opencode(24, 80);
    let mut model = ScreenModel::new(24, 80, 0, regions);
    model.process(b"Hello");
    model.resize(30, 120);
    assert_eq!(model.prev_content_rows.len(), 28);
}
