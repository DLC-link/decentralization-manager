//! Read-only popups: chain audit, JSON, messages and feed detail.

use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use common::types::{PendingInvitation, WAITING_FOR_PEERS_STEP, WorkflowRole, WorkflowRun};

use crate::api::{ChainAuditEntry, FeedItem};
use crate::logo;

use crate::ui::json::json_lines;
use crate::ui::tabs::workflow_status_display;
use crate::ui::widgets::truncate;
use crate::ui::widgets::{centered_rect, dim_line, draw_shadow, format_timestamp, popup_block};

/// A scrollable popup of the on-chain governance audit trail.
pub fn chain_audit_popup(frame: &mut Frame, area: Rect, entries: &[ChainAuditEntry], scroll: u16) {
    let lines: Vec<Line> = if entries.is_empty() {
        vec![dim_line("(no on-chain audit entries)")]
    } else {
        entries
            .iter()
            .map(|entry| {
                let summary = if entry.action_summary.is_empty() {
                    entry
                        .choice
                        .clone()
                        .unwrap_or_else(|| entry.event_type.clone())
                } else {
                    entry.action_summary.clone()
                };
                Line::from(vec![
                    Span::styled(
                        format!("{:<17}", format_timestamp(entry.timestamp)),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{:<9}", truncate(&entry.event_type, 9)),
                        Style::default().fg(chain_event_color(&entry.event_type)),
                    ),
                    Span::styled(
                        format!("{:<12}", truncate(&entry.governance_type, 12)),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(summary),
                ])
            })
            .collect()
    };
    let width = 84.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16).saturating_add(2)).clamp(6, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .block(popup_block("On-chain audit"));
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// Color for an on-chain audit event type.
fn chain_event_color(event_type: &str) -> Color {
    match event_type {
        "execute" => Color::Green,
        "propose" | "confirm" => logo::ORANGE,
        "cancel" | "expire" => Color::Red,
        _ => Color::Gray,
    }
}

/// A scrollable popup showing syntax-highlighted JSON (audit details).
pub fn json_popup(frame: &mut Frame, area: Rect, value: &serde_json::Value, scroll: u16) {
    let lines = json_lines(value);
    let width = 72.min(area.width.saturating_sub(4));
    let height = (u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2))
    .clamp(5, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .block(popup_block("Audit details"));
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// A small centered popup with a wrapped message.
pub fn message_popup(frame: &mut Frame, area: Rect, title: &str, message: &str, color: Color) {
    let width = (message.chars().count() as u16 + 6).clamp(28, area.width.saturating_sub(6));
    let rect = centered_rect(width, 7, area);
    let paragraph = Paragraph::new(message.to_owned())
        .style(Style::default().fg(color))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(popup_block(title));
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// A scrollable detail popup for a feed item (workflow run or invitation).
pub fn feed_detail_popup(frame: &mut Frame, area: Rect, item: &FeedItem, scroll: u16) {
    let (title, lines) = match item {
        FeedItem::Run(run) => (format!("{} workflow", run.kind), run_detail_lines(run)),
        FeedItem::Invitation(invitation) => (
            format!("{} invitation", invitation.invitation_type),
            invitation_detail_lines(invitation),
        ),
    };
    let width = 84.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16).saturating_add(2)).clamp(6, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .wrap(Wrap { trim: false })
        .block(popup_block(&title));
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// A right-padded dim field label for the detail popups.
pub fn detail_label(text: &str) -> Span<'static> {
    Span::styled(format!("{text:<15}"), Style::default().fg(Color::DarkGray))
}

/// A `label  value` detail line.
pub fn detail_kv(label: &str, value: String) -> Line<'static> {
    Line::from(vec![detail_label(label), Span::raw(value)])
}

/// The peer count for a run's detail popup, or `None` when there is nothing
/// truthful to show. Coordinator-only: a peer-side row never learns what the
/// other peers did, so its counter would sit at 0 for the whole run. While the
/// coordinator waits, the peers that joined are the progress — a waiting step
/// carries no command, so nothing completes it — and from the next step on it
/// is the peers that completed the current one.
fn peers_progress(run: &WorkflowRun) -> Option<String> {
    if run.role != WorkflowRole::Coordinator || run.expected_peers.is_empty() {
        return None;
    }
    let (done, label) = if run.current_step == WAITING_FOR_PEERS_STEP {
        (run.connected_peers.len(), "joined")
    } else {
        (run.completed_peers.len(), "done")
    };
    Some(format!(
        "{done}/{total} {label}",
        total = run.expected_peers.len()
    ))
}

/// An indented detail value line (for list items).
pub fn detail_item(value: String) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(value, Style::default().fg(Color::Gray)),
    ])
}

/// Detail lines for a workflow run.
fn run_detail_lines(run: &WorkflowRun) -> Vec<Line<'static>> {
    let (color, label) = workflow_status_display(run.status);
    let mut lines = vec![
        detail_kv("Instance", run.instance_name.clone()),
        detail_kv("Kind", run.kind.to_string()),
        detail_kv("Role", run.role.to_string()),
        Line::from(vec![
            detail_label("Status"),
            Span::styled(label.to_owned(), Style::default().fg(color)),
        ]),
        detail_kv(
            "Step",
            format!(
                "{step} ({index}/{total})",
                step = if run.current_step.is_empty() {
                    "—"
                } else {
                    &run.current_step
                },
                index = run.step_index,
                total = run.step_total
            ),
        ),
    ];
    if let Some(prefix) = run.prefix.as_deref().filter(|p| !p.is_empty()) {
        lines.push(detail_kv("Prefix", prefix.to_owned()));
    }
    if let Some(dec_party) = &run.dec_party_id {
        lines.push(detail_kv("Dec party", dec_party.to_string()));
    }
    if run.previous_threshold.is_some() || run.new_threshold.is_some() {
        lines.push(detail_kv(
            "Threshold",
            format!(
                "{prev} → {next}",
                prev = run.previous_threshold.unwrap_or(0),
                next = run.new_threshold.unwrap_or(0)
            ),
        ));
    }
    if let Some(kicked) = &run.kicked_participant {
        lines.push(detail_kv("Kicked", kicked.to_string()));
    }
    if !run.participants.is_empty() {
        lines.push(detail_kv("Participants", String::new()));
        lines.extend(run.participants.iter().map(|p| detail_item(p.to_string())));
    }
    if let Some(peers) = peers_progress(run) {
        lines.push(detail_kv("Peers", peers));
    }
    if !run.package_names.is_empty() {
        lines.push(detail_kv("Packages", run.package_names.join(", ")));
    }
    if !run.dar_filenames.is_empty() {
        lines.push(detail_kv("DARs", run.dar_filenames.join(", ")));
    }
    if let Some(error) = &run.error {
        lines.push(Line::default());
        lines.push(Line::styled(
            format!("Error: {error}"),
            Style::default().fg(Color::Red),
        ));
    }
    lines
}

/// Detail lines for a pending invitation.
fn invitation_detail_lines(invitation: &PendingInvitation) -> Vec<Line<'static>> {
    let mut lines = vec![detail_kv("Type", invitation.invitation_type.to_string())];
    if let Some(name) = &invitation.coordinator_name {
        lines.push(detail_kv("From", name.clone()));
    }
    lines.push(detail_kv(
        "Coordinator",
        invitation.coordinator_pubkey.clone(),
    ));
    if let Some(prefix) = invitation.prefix.as_deref().filter(|p| !p.is_empty()) {
        lines.push(detail_kv("Prefix", prefix.to_owned()));
    }
    if let Some(dec_party) = &invitation.dec_party_id {
        lines.push(detail_kv("Dec party", dec_party.to_string()));
    }
    if invitation.previous_threshold.is_some() || invitation.new_threshold.is_some() {
        lines.push(detail_kv(
            "Threshold",
            format!(
                "{prev} → {next}",
                prev = invitation.previous_threshold.unwrap_or(0),
                next = invitation.new_threshold.unwrap_or(0)
            ),
        ));
    }
    if let Some(kicked) = &invitation.kicked_participant {
        lines.push(detail_kv("Kicked", kicked.to_string()));
    }
    if !invitation.participants.is_empty() {
        lines.push(detail_kv("Participants", String::new()));
        lines.extend(
            invitation
                .participants
                .iter()
                .map(|p| detail_item(p.to_string())),
        );
    }
    if !invitation.package_names.is_empty() {
        lines.push(detail_kv("Packages", invitation.package_names.join(", ")));
    }
    if !invitation.dar_filenames.is_empty() {
        // Show the pinned content hash beside each filename. The peer refuses
        // any DAR whose bytes do not hash to this value, so it is the operator
        // accepting the content — not just the name — and it can be compared
        // against a published release hash before accepting.
        lines.push(detail_kv("DARs", String::new()));
        for (index, filename) in invitation.dar_filenames.iter().enumerate() {
            lines.push(detail_item(match invitation.dar_hashes.get(index) {
                Some(hash) => format!("{filename}  sha256:{hash}"),
                None => format!("{filename}  (no hash pinned)"),
            }));
        }
    }
    lines
}

#[cfg(test)]
mod tests {

    use common::types::WorkflowKind;

    use common::types::{WAITING_FOR_PEERS_STEP, WorkflowProgress, WorkflowRole, WorkflowRun};

    use crate::api::{ChainAuditEntry, FeedItem};
    use crate::app::Overlay;

    use super::*;
    use crate::ui::fixtures::{canton_id, render};
    use crate::ui::frame::draw_overlay;

    #[test]
    fn chain_audit_popup_renders_entries() {
        let overlay = Overlay::ChainAudit {
            entries: vec![ChainAuditEntry {
                timestamp: 1_750_000_000,
                event_type: "execute".to_owned(),
                governance_type: "core_domain".to_owned(),
                action_summary: "Executed SetThreshold".to_owned(),
                choice: Some("GovernanceRules_Execute".to_owned()),
            }],
            scroll: 0,
        };
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("On-chain audit"));
        assert!(rendered.contains("execute"));
        assert!(rendered.contains("core_domain"));
        assert!(rendered.contains("Executed SetThreshold"));
    }

    #[test]
    fn audit_json_modal_renders_highlighted() {
        let overlay = Overlay::Json {
            value: serde_json::json!({ "new_threshold": 3 }),
            scroll: 0,
        };
        let rendered = render(|frame, area| {
            draw_overlay(frame, area, &overlay, "⠋");
        });
        // The modal shows its title and the pretty-printed JSON details.
        assert!(rendered.contains("Audit details"));
        assert!(rendered.contains("new_threshold"));
    }

    #[test]
    fn peers_progress_counts_joins_while_waiting_then_completions() {
        let mut run = WorkflowRun {
            instance_name: "beth-network-creation".to_owned(),
            kind: WorkflowKind::Onboarding,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::InProgress,
            current_step: WAITING_FOR_PEERS_STEP.to_owned(),
            step_index: 0,
            step_total: 7,
            config_json: String::new(),
            coordinator_pubkey: None,
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers: vec![canton_id("p1"), canton_id("p2"), canton_id("p3")],
            completed_peers: vec![canton_id("p1")],
            connected_peers: vec![canton_id("p1"), canton_id("p2")],
            acs_progress: None,
            dec_party_id: None,
            prefix: Some("beth-network".to_owned()),
            participants: Vec::new(),
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
            kicked_owner_key: None,
            added_participant: None,
            package_names: Vec::new(),
            dar_filenames: Vec::new(),
            error: None,
            dismissed: false,
            created_at: 0,
            updated_at: 0,
        };

        // Waiting: the joins are the progress. Reporting `completed_peers` here
        // is what showed a zero to an operator whose peers had accepted.
        assert_eq!(peers_progress(&run).as_deref(), Some("2/3 joined"));

        // Past the gate, completions of the current step take over.
        run.current_step = "SignDns".to_owned();
        assert_eq!(peers_progress(&run).as_deref(), Some("1/3 done"));

        // A peer-side row has no such knowledge, so it shows no count at all.
        run.role = WorkflowRole::Peer;
        assert_eq!(peers_progress(&run), None);
    }

    #[test]
    fn feed_detail_popup_renders_run_fields_and_error() {
        let run = WorkflowRun {
            instance_name: "onboarding-treasury-abc".to_owned(),
            kind: WorkflowKind::Onboarding,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::Failed,
            current_step: "WaitingForPeers".to_owned(),
            step_index: 1,
            step_total: 7,
            config_json: String::new(),
            coordinator_pubkey: None,
            coordinator_instance: None,
            coordinator_name: None,
            expected_peers: Vec::new(),
            completed_peers: Vec::new(),
            connected_peers: Vec::new(),
            acs_progress: None,
            dec_party_id: None,
            prefix: Some("treasury".to_owned()),
            participants: Vec::new(),
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
            kicked_owner_key: None,
            added_participant: None,
            package_names: Vec::new(),
            dar_filenames: Vec::new(),
            error: Some("peer unreachable".to_owned()),
            dismissed: false,
            created_at: 0,
            updated_at: 0,
        };
        let overlay = Overlay::FeedDetail {
            item: Box::new(FeedItem::Run(run)),
            scroll: 0,
        };
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Onboarding workflow"));
        assert!(rendered.contains("Instance"));
        assert!(rendered.contains("Failed"));
        assert!(rendered.contains("Error:"));
    }
}
