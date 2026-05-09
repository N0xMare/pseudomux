pub mod classifier;
pub mod content_buffer;
pub mod content_filter;
pub mod differ;
pub mod regions;
pub mod screen;
pub mod watch;

pub use classifier::{AgentState, RegionClassifier, SemanticEvent, StatusPatterns};
pub use content_buffer::{ContentBuffer, ContentEntry, ContentTag};
pub use content_filter::ContentFilter;
pub use differ::ScreenChange;
pub use regions::ScreenRegions;
pub use screen::ScreenModel;
pub use watch::{WatchEvent, WatchEventBuilder};

pub use content_filter::{deduplicate_lines, extract_response_text, is_claude_code_chrome};
