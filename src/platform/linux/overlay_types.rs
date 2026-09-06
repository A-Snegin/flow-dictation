//! Temporary home for the overlay's shared types on Linux, until the overlay
//! agent extracts `overlay_model.rs` for both platforms.

use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayState {
    Hidden,
    Listening,
    Finalising,
    Done,
    Error,
}

pub const LISTENING_TICK: Duration = Duration::from_millis(40);
