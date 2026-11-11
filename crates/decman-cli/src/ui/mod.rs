//! The terminal UI: the frame, the tab panels, the party detail view and the
//! overlay popups.

mod detail;
#[cfg(test)]
mod fixtures;
mod frame;
mod json;
mod login;
mod popups;
mod tabs;
mod widgets;

pub use frame::draw;
pub use login::draw_login;
