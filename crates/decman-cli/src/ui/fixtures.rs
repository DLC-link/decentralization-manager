//! Fixtures shared by the ui tests.

use common::canton_id::CantonId;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::{Frame, Terminal};

/// A valid 34-byte (68 hex char) namespace for building realistic Canton ids.
pub const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

/// A second valid namespace, distinct from [`NS`], for participant ids.
pub const NS2: &str = "1220034c3a6a945442fb9f9b3f8e6a3f5e8c7d6b5a4938271605f4e3d2c1b0a99887";

pub fn canton_id(prefix: &str) -> CantonId {
    CantonId::parse(&format!("{prefix}::{NS}")).unwrap()
}

pub fn render(mut body: impl FnMut(&mut Frame, Rect)) -> String {
    let mut terminal = Terminal::new(TestBackend::new(96, 16)).unwrap();
    terminal.draw(|frame| body(frame, frame.area())).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
