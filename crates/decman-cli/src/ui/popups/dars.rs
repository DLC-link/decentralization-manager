//! DAR popups: the package comparison and the peer select.

use std::collections::HashSet;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use common::types::{PackageInfo, PeerPackageComparison};

use crate::app::PeerChoice;
use crate::logo;

use crate::ui::tabs::peer_error_label;
use crate::ui::widgets::{centered_rect, draw_shadow, header_style, popup_block, truncate};

/// The package-checker popup: local count and each peer's sync state.
pub fn compare_popup(
    frame: &mut Frame,
    area: Rect,
    comparison: &PeerPackageComparison,
    scroll: u16,
) {
    /// A peer rendered as a matrix column: its label, the set of
    /// `(name, version)` it vets, and how many local packages it is missing.
    struct PeerCol {
        label: String,
        width: usize,
        reachable: bool,
        error: &'static str,
        keys: HashSet<(String, String)>,
        missing: usize,
    }

    let key_of = |pkg: &PackageInfo| (pkg.name.clone(), pkg.version.clone());
    let peer_cols: Vec<PeerCol> = comparison
        .peers
        .iter()
        .map(|peer| {
            let keys: HashSet<(String, String)> = peer.packages.iter().map(key_of).collect();
            let missing = comparison
                .local_packages
                .iter()
                .filter(|pkg| !keys.contains(&key_of(pkg)))
                .count();
            let label = truncate(&peer.name, 12);
            let width = label.chars().count().max(3);
            PeerCol {
                label,
                width,
                reachable: peer.reachable,
                error: peer.error_kind.map_or("unreachable", peer_error_label),
                keys,
                missing,
            }
        })
        .collect();

    let name_w = 32usize;
    let ver_w = 9usize;

    // Header: total, then one status chip per peer.
    let mut lines = vec![Line::from(Span::styled(
        format!("Local packages: {}", comparison.local_packages.len()),
        Style::default().add_modifier(Modifier::BOLD),
    ))];
    let mut summary: Vec<Span> = Vec::new();
    for (index, (peer, col)) in comparison.peers.iter().zip(&peer_cols).enumerate() {
        if index > 0 {
            summary.push(Span::raw("   "));
        }
        let (color, note) = if !col.reachable {
            (Color::Red, format!("unreachable ({})", col.error))
        } else if col.missing == 0 {
            (Color::Green, "in sync".to_owned())
        } else {
            (Color::Yellow, format!("{} missing", col.missing))
        };
        summary.push(Span::styled("● ", Style::default().fg(color)));
        summary.push(Span::raw(format!("{} ", peer.name)));
        summary.push(Span::styled(note, Style::default().fg(color)));
    }
    lines.push(Line::from(summary));
    lines.push(Line::default());

    // Matrix header row: PACKAGE · VERSION · one column per peer.
    let mut header = vec![
        Span::styled(format!("{:<name_w$}", "PACKAGE"), header_style()),
        Span::styled(format!("{:<ver_w$}", "VERSION"), header_style()),
    ];
    for col in &peer_cols {
        header.push(Span::styled(
            format!(" {:^width$}", col.label, width = col.width),
            header_style(),
        ));
    }
    lines.push(Line::from(header));

    // One row per local package (sorted like the Dars tab), with a ✓ / ✗ / –
    // per peer: vetted (green) / missing-or-mismatch (yellow) / unreachable.
    let mut locals: Vec<&PackageInfo> = comparison.local_packages.iter().collect();
    locals.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.version.cmp(&b.version)));
    for pkg in locals {
        let key = key_of(pkg);
        let mut spans = vec![
            Span::raw(format!("{:<name_w$}", truncate(&pkg.name, name_w - 1))),
            Span::styled(
                format!("{:<ver_w$}", truncate(&pkg.version, ver_w - 1)),
                Style::default().fg(Color::DarkGray),
            ),
        ];
        for col in &peer_cols {
            let (symbol, color) = if !col.reachable {
                ("–", Color::DarkGray)
            } else if col.keys.contains(&key) {
                ("✓", Color::Green)
            } else {
                ("✗", Color::Yellow)
            };
            spans.push(Span::styled(
                format!(" {:^width$}", symbol, width = col.width),
                Style::default().fg(color),
            ));
        }
        lines.push(Line::from(spans));
    }

    let width = 92.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16) + 2)
        .min(area.height.saturating_sub(4))
        .max(6);
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .block(popup_block("Package check"));
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The DAR distribution popup: a tickable list of peers.
pub fn peer_select_popup(
    frame: &mut Frame,
    area: Rect,
    filename: &str,
    peers: &[PeerChoice],
    cursor: usize,
) {
    let lines: Vec<Line> = peers
        .iter()
        .enumerate()
        .map(|(i, peer)| {
            let marker = if i == cursor { "▶ " } else { "  " };
            let check = if peer.checked { "[x] " } else { "[ ] " };
            let style = if i == cursor {
                Style::default()
                    .fg(logo::ORANGE)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(format!("{marker}{check}{}", peer.name), style))
        })
        .collect();

    let width = 60.min(area.width.saturating_sub(4));
    let height = ((peers.len() as u16) + 2).clamp(6, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let title = format!("Distribute {filename}");
    let paragraph = Paragraph::new(lines).block(popup_block(&title));
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

#[cfg(test)]
mod tests {

    use common::types::PeerPackageResult;

    use common::types::{PackageInfo, PeerPackageComparison};

    use crate::app::Overlay;

    use crate::ui::fixtures::render;
    use crate::ui::frame::draw_overlay;

    fn pkg(name: &str, version: &str) -> PackageInfo {
        PackageInfo {
            package_id: format!("00{name}"),
            name: name.to_owned(),
            version: version.to_owned(),
        }
    }

    #[test]
    fn compare_popup_renders_matrix_of_marks() {
        let comparison = PeerPackageComparison {
            local_packages: vec![pkg("cbtc", "1.0.0"), pkg("treasury", "0.0.1")],
            peers: vec![
                PeerPackageResult {
                    participant_id: "p1::1220".to_owned(),
                    name: "devnet 1".to_owned(),
                    reachable: true,
                    error_kind: None,
                    // Has cbtc but is missing treasury.
                    packages: vec![pkg("cbtc", "1.0.0")],
                },
                PeerPackageResult {
                    participant_id: "p2::1220".to_owned(),
                    name: "devnet 4".to_owned(),
                    reachable: false,
                    error_kind: None,
                    packages: Vec::new(),
                },
            ],
        };
        let overlay = Overlay::Compare {
            comparison,
            scroll: 0,
        };
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));

        assert!(rendered.contains("Local packages: 2"));
        assert!(rendered.contains("PACKAGE"));
        assert!(rendered.contains("cbtc"));
        // devnet 1: cbtc ✓, treasury ✗; devnet 4: unreachable → – in every cell.
        assert!(rendered.contains('✓'));
        assert!(rendered.contains('✗'));
        assert!(rendered.contains('–'));
        assert!(rendered.contains("1 missing"));
        assert!(rendered.contains("unreachable"));
    }
}
