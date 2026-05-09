use pseudomux_core::vte::ScreenRegions;

#[test]
fn opencode_regions() {
    let r = ScreenRegions::opencode(24, 80);
    assert_eq!(r.content_start, 0);
    assert_eq!(r.content_end, 22);
    assert_eq!(r.status_start, 22);
    assert_eq!(r.status_end, 24);
    assert!(r.is_content(0));
    assert!(r.is_content(21));
    assert!(!r.is_content(22));
    assert!(r.is_status(22));
    assert!(r.is_status(23));
    assert!(!r.is_status(0));
}

#[test]
fn full_screen_regions() {
    let r = ScreenRegions::full_screen(24, 80);
    assert_eq!(r.content_end, 24);
    assert_eq!(r.status_start, 24);
    assert!(r.is_content(23));
    assert!(!r.is_status(23));
}

#[test]
fn resize_opencode() {
    let mut r = ScreenRegions::opencode(24, 80);
    r.resize(30, 120);
    assert_eq!(r.content_end, 28);
    assert_eq!(r.status_start, 28);
    assert_eq!(r.status_end, 30);
    assert_eq!(r.cols, 120);
}

#[test]
fn resize_full_screen() {
    let mut r = ScreenRegions::full_screen(24, 80);
    r.resize(30, 120);
    assert_eq!(r.content_end, 30);
    assert_eq!(r.status_start, 30);
    assert_eq!(r.status_end, 30);
}

#[test]
fn edge_case_single_row() {
    let r = ScreenRegions::opencode(1, 80);
    assert_eq!(r.content_start, 0);
    assert_eq!(r.content_end, 0);
    assert_eq!(r.status_start, 0);
    assert_eq!(r.status_end, 1);
    assert!(!r.is_content(0));
    assert!(r.is_status(0));
}

#[test]
fn edge_case_two_rows() {
    let r = ScreenRegions::opencode(2, 80);
    assert_eq!(r.content_start, 0);
    assert_eq!(r.content_end, 0);
    assert_eq!(r.status_start, 0);
    assert_eq!(r.status_end, 2);
}

#[test]
fn opencode_40_row_layout() {
    let r = ScreenRegions::opencode(40, 120);
    assert_eq!(r.content_end, 38);
    assert_eq!(r.status_start, 38);
    assert_eq!(r.status_end, 40);
    assert!(r.is_content(37));
    assert!(r.is_status(38));
    assert!(r.is_status(39));
}

#[test]
fn claude_code_regions() {
    let r = ScreenRegions::claude_code(50, 120);
    assert_eq!(r.content_end, 46);
    assert_eq!(r.status_start, 46);
    assert_eq!(r.status_end, 50);
    assert_eq!(r.rows, 50);
    assert_eq!(r.cols, 120);
}

#[test]
fn claude_code_resize() {
    let mut r = ScreenRegions::claude_code(50, 120);
    r.resize(60, 140);
    assert_eq!(r.content_end, 56);
    assert_eq!(r.status_start, 56);
    assert_eq!(r.rows, 60);
    assert_eq!(r.cols, 140);
}
