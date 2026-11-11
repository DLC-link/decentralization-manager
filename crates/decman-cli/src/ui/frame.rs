//! The frame: layout, header, footer and the overlay dispatcher.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::app::{App, MemberChange, Overlay, Tab, TabView};
use crate::logo;

use crate::ui::detail::draw_party_detail;
use crate::ui::popups::{auth_popup, chain_audit_popup, change_threshold_popup};
use crate::ui::popups::{compare_popup, composer_pick_popup, composer_popup, deploy_popup};
use crate::ui::popups::{feed_detail_popup, governance_popup, grant_popup, json_popup};
use crate::ui::popups::{member_change_popup, message_popup, network_edit_popup, node_info_popup};
use crate::ui::popups::{onboard_popup, party_config_popup, peer_select_popup};
use crate::ui::tabs::{draw_dars, draw_feed, draw_parties, draw_peers, summary_line, tab_block};

/// Subtitle shown beneath the wordmark, matching the web app's branding.
const SUBTITLE: &str = "Decentralization Manager";

/// Braille spinner frames cycled while a tab is loading.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Render the whole UI: branded header, the active tab's panel, and a footer.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(7),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // Record the panel rect so mouse clicks can be mapped back to a tab / row.
    app.set_body_area(body);

    draw_header(frame, header, app.node_version());

    let spinner = SPINNER[app.tick() % SPINNER.len()];

    // The party detail view replaces the tabs while open.
    if app.detail_open() {
        if let Some((party, data, audit_state)) = app.detail_view() {
            draw_party_detail(frame, body, party, data, audit_state);
        }
        let hint = match app.overlay() {
            Overlay::Json { .. } | Overlay::ChainAudit { .. } => " ↑/↓ scroll · esc close",
            Overlay::MemberChange(form) => member_change_hint(form.kind),
            Overlay::ChangeThreshold(_) => " ←/→ threshold · enter apply · esc cancel",
            Overlay::Governance(_) => {
                " ↑/↓ · c confirm · e exec · r revoke · x expire · n new · p propose · esc"
            }
            Overlay::ComposerPick(_) => " ↑/↓ pick · enter open · esc cancel",
            Overlay::Composer(_) => {
                " ↑/↓ field · type · ←/→ toggle · enter next/submit · esc cancel"
            }
            Overlay::Deploy(_) => " ↑/↓ field · type number · enter next/deploy · esc cancel",
            Overlay::PartyConfig(_) => {
                " ↑/↓ field · ←/→ provider · type · enter next/run · esc cancel"
            }
            Overlay::Message(_) | Overlay::Busy(_) => " enter / esc to close",
            _ => {
                " ↑/↓ audit · enter json · a login · g gov · D deploy · c chain · K kick · \
                 P add · T thresh · esc back"
            }
        };
        frame.render_widget(
            Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
            footer,
        );
        draw_overlay(frame, frame.area(), app.overlay(), spinner);
        return;
    }

    let active = app.active_tab();
    let search = app.search_hint();
    let hint = footer_hint(active, app.overlay(), app.can_logout());
    let summary = match app.tab_view() {
        TabView::Parties(status, parties, state) => {
            draw_parties(
                frame,
                body,
                status,
                &parties,
                state,
                tab_block(active, search),
                spinner,
            );
            summary_line(status, parties.len(), "parties")
        }
        TabView::Peers(status, peers, state) => {
            draw_peers(
                frame,
                body,
                status,
                peers,
                state,
                tab_block(active, search),
                spinner,
            );
            summary_line(status, peers.len(), "peers")
        }
        TabView::Dars(status, dars, state) => {
            draw_dars(
                frame,
                body,
                status,
                &dars,
                state,
                tab_block(active, search),
                spinner,
            );
            summary_line(status, dars.len(), "packages")
        }
        TabView::Workflows(status, feed, state) => {
            draw_feed(
                frame,
                body,
                status,
                feed,
                state,
                tab_block(active, search),
                spinner,
            );
            summary_line(status, feed.len(), "items")
        }
    };

    draw_footer(frame, footer, &hint, &summary);
    draw_overlay(frame, frame.area(), app.overlay(), spinner);
}

/// Draw the centered BitSafe wordmark, subtitle, and — once connected — the
/// version of the node this session is managing (the login screen has no node
/// yet, so `version` is `None` there).
pub fn draw_header(frame: &mut Frame, area: Rect, version: Option<&str>) {
    let mut lines = logo::lines();
    lines.push(Line::styled(
        SUBTITLE,
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    ));
    if let Some(version) = version {
        lines.push(Line::styled(
            format!("v{version}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

/// The footer hint of the kick and add-party forms.
fn member_change_hint(kind: MemberChange) -> &'static str {
    match kind {
        MemberChange::Kick => " ↑/↓ pick · ←/→ threshold · enter kick · esc cancel",
        MemberChange::Add => " ↑/↓ pick · ←/→ threshold · enter add · esc cancel",
    }
}

/// Context-sensitive key hints for the footer, based on the active tab, any
/// open overlay, and whether logout is available.
fn footer_hint(active: Tab, overlay: &Overlay, can_logout: bool) -> String {
    match overlay {
        Overlay::PeerSelect { .. } => {
            " ↑/↓ move · space toggle · enter distribute · esc cancel".to_owned()
        }
        Overlay::Compare { .. } => " ↑/↓ scroll · esc close".to_owned(),
        Overlay::Json { .. } | Overlay::FeedDetail { .. } | Overlay::ChainAudit { .. } => {
            " ↑/↓ scroll · esc close".to_owned()
        }
        Overlay::Onboard(_) => {
            " ↑/↓ field · space toggle peer · enter start · esc cancel".to_owned()
        }
        Overlay::MemberChange(form) => member_change_hint(form.kind).to_owned(),
        Overlay::ChangeThreshold(_) => " ←/→ threshold · enter apply · esc cancel".to_owned(),
        Overlay::Auth { .. } => " ↑/↓ select · t test · g grant · e config · esc close".to_owned(),
        Overlay::GrantRights(_) => " ↑/↓ field · type · enter next/grant · esc cancel".to_owned(),
        Overlay::PartyConfig(_) => {
            " ↑/↓ field · ←/→ provider · type · enter next/run · esc cancel".to_owned()
        }
        Overlay::Governance(_) => {
            " ↑/↓ · c confirm · e exec · r revoke · x expire · n new · p propose · esc".to_owned()
        }
        Overlay::ComposerPick(_) => " ↑/↓ pick · enter open · esc cancel".to_owned(),
        Overlay::Composer(_) => {
            " ↑/↓ field · type to edit · ←/→ toggle · enter next/submit · esc cancel".to_owned()
        }
        Overlay::Deploy(_) => {
            " ↑/↓ field · type number · enter next/deploy · esc cancel".to_owned()
        }
        Overlay::NetworkEdit(state) => if state.adding.is_some() {
            " ↑/↓ field · type · enter next/add · esc back"
        } else {
            " ↑/↓ select · a add · d remove · s save · esc cancel"
        }
        .to_owned(),
        Overlay::NodeInfo(_) => " esc close".to_owned(),
        Overlay::Message(_) => " enter / esc to close".to_owned(),
        Overlay::Busy(_) => " working… · enter/esc to dismiss".to_owned(),
        Overlay::None => {
            let base = match active {
                Tab::Parties => " ↑↓ nav · enter view · n onboard · r refresh",
                Tab::Peers => " ↑↓ nav · e edit · i info · tab switch",
                Tab::Dars => " c check · u upload · d distribute",
                Tab::Workflows => {
                    " a accept · x deny · c cancel · t retry · enter detail · d dismiss"
                }
            };
            let tail = if can_logout {
                " · A auth · esc logout · q quit"
            } else {
                " · A auth · q quit"
            };
            format!("{base}{tail}")
        }
    }
}

/// Draw the footer: key hints on the left, the active tab's summary on the right.
fn draw_footer(frame: &mut Frame, area: Rect, hint: &str, summary: &str) {
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(14)]).areas(area);

    let dim = Style::default().fg(Color::DarkGray);
    frame.render_widget(Paragraph::new(hint.to_owned()).style(dim), left);
    frame.render_widget(
        Paragraph::new(summary.to_owned())
            .style(dim)
            .alignment(Alignment::Right),
        right,
    );
}

/// Draw the active modal overlay (if any) over the rest of the UI.
pub fn draw_overlay(frame: &mut Frame, area: Rect, overlay: &Overlay, spinner: &str) {
    match overlay {
        Overlay::None => {}
        Overlay::Busy(message) => {
            message_popup(
                frame,
                area,
                "Working",
                &format!("{spinner} {message}"),
                Color::Gray,
            );
        }
        Overlay::Message(message) => message_popup(frame, area, "Result", message, Color::White),
        Overlay::Compare { comparison, scroll } => {
            compare_popup(frame, area, comparison, *scroll);
        }
        Overlay::PeerSelect { dar, peers, cursor } => {
            peer_select_popup(frame, area, &dar.filename, peers, *cursor);
        }
        Overlay::Json { value, scroll } => json_popup(frame, area, value, *scroll),
        Overlay::Onboard(form) => onboard_popup(frame, area, form),
        Overlay::MemberChange(form) => member_change_popup(frame, area, form),
        Overlay::ChangeThreshold(form) => change_threshold_popup(frame, area, form),
        Overlay::FeedDetail { item, scroll } => feed_detail_popup(frame, area, item, *scroll),
        Overlay::ChainAudit { entries, scroll } => chain_audit_popup(frame, area, entries, *scroll),
        Overlay::Auth { parties, selected } => auth_popup(frame, area, parties, *selected),
        Overlay::GrantRights(form) => grant_popup(frame, area, form),
        Overlay::Governance(view) => governance_popup(frame, area, view),
        Overlay::ComposerPick(pick) => composer_pick_popup(frame, area, pick),
        Overlay::Composer(composer) => composer_popup(frame, area, composer),
        Overlay::Deploy(form) => deploy_popup(frame, area, form),
        Overlay::NetworkEdit(state) => network_edit_popup(frame, area, state),
        Overlay::PartyConfig(form) => party_config_popup(frame, area, form),
        Overlay::NodeInfo(data) => node_info_popup(frame, area, data),
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::ui::fixtures::render;

    #[test]
    fn header_shows_node_version_only_when_connected() {
        let with = render(|frame, area| draw_header(frame, area, Some("1.2.1")));
        assert!(with.contains("Decentralization Manager"));
        assert!(with.contains("v1.2.1"));

        // The login screen passes None — no version line.
        let without = render(|frame, area| draw_header(frame, area, None));
        assert!(without.contains("Decentralization Manager"));
        assert!(!without.contains("v1.2.1"));
    }
}
