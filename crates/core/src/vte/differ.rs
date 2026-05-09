use serde::{Deserialize, Serialize};

/// A change detected in the VTE screen model between two successive renders.
///
/// Produced by [`crate::vte::screen::ScreenModel::process`] and consumed by
/// [`crate::vte::classifier::RegionClassifier::classify`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ScreenChange {
    /// A row in the content region changed.
    ContentRowChanged { row: u16, old: String, new: String },
    /// The status-bar region changed.
    StatusBarChanged { old: String, new: String },
    /// The screen was fully cleared (e.g. due to a full redraw / `ED 2` sequence).
    ScreenCleared,
}
