//! The party detail view and its boxes.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState};

use common::types::DecentralizedParty;

use crate::api::{Holding, audit_action, party_name};
use crate::app::DetailData;
use crate::logo;

use crate::ui::widgets::{audit_status_color, dim_line, fixed, fixed_right, format_timestamp};
use crate::ui::widgets::{header_style, highlight_style, permission_color, popup_block};
use crate::ui::widgets::{truncate, truncate_middle};

/// Draw the party detail view: each section in its own framed box, with the
/// audit box a selectable table that fills the remaining space.
pub fn draw_party_detail(
    frame: &mut Frame,
    area: Rect,
    party: &DecentralizedParty,
    data: Option<&DetailData>,
    audit_state: &mut TableState,
) {
    let [summary, participants, contracts, holdings, audit] = Layout::vertical([
        Constraint::Length(summary_height(party, data)),
        Constraint::Length(box_height(party.participants.len(), 5)),
        Constraint::Length(box_height(party.contracts.len(), 5)),
        // +1 content row for the holdings header line.
        Constraint::Length(box_height(holdings_count(data) + 1, 6)),
        Constraint::Min(4),
    ])
    .areas(area);

    draw_summary_box(frame, summary, party, data);
    draw_participants_box(frame, participants, party);
    draw_contracts_box(frame, contracts, party);
    draw_holdings_box(frame, holdings, data);
    draw_audit_box(frame, audit, data, audit_state);
}

/// Height for a list section's box: one row per item (min one), plus borders,
/// capped so the audit box always keeps room.
fn box_height(items: usize, cap: u16) -> u16 {
    (u16::try_from(items).unwrap_or(cap).max(1) + 2).clamp(3, cap)
}

/// Holdings count for sizing the box (1 while loading / empty / errored).
fn holdings_count(data: Option<&DetailData>) -> usize {
    data.and_then(|data| data.holdings.as_ref().ok())
        .map_or(1, |holdings| holdings.len().max(1))
}

/// Height of the summary box, accounting for the optional owner-key and
/// governance-state lines.
fn summary_height(party: &DecentralizedParty, data: Option<&DetailData>) -> u16 {
    let mut lines = 2u16;
    if party.my_owner_key.is_some() {
        lines += 1;
    }
    match data.map(|d| &d.gov_state) {
        Some(Ok(Some(state))) => {
            lines += 1;
            if state.out_of_date {
                lines += 1;
            }
        }
        // Reserve a line to surface a governance-state load error.
        Some(Err(_)) => lines += 1,
        _ => {}
    }
    lines + 2
}

/// Format a microsecond duration as a compact human string (e.g. `24h`, `30m`).
/// Days are only used for 2+ whole days, so a one-day timeout reads as `24h`
/// (matching the web app's rel-time labels).
pub fn format_micros_human(micros: i64) -> String {
    let secs = micros / 1_000_000;
    if secs >= 172_800 && secs % 86_400 == 0 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 && secs % 3_600 == 0 {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 && secs % 60 == 0 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

/// The party summary box: id, threshold/owners/participants/contracts, key, and
/// on-ledger governance state (threshold, members, action timeout) when loaded.
fn draw_summary_box(
    frame: &mut Frame,
    area: Rect,
    party: &DecentralizedParty,
    data: Option<&DetailData>,
) {
    let label = |text: &'static str| Span::styled(text, Style::default().fg(Color::DarkGray));
    let mut lines = vec![
        Line::from(vec![
            label("Party id  "),
            Span::raw(party.party_id.to_string()),
        ]),
        Line::from(vec![
            label("Threshold "),
            Span::raw(party.threshold.to_string()),
            label("   Owners "),
            Span::raw(party.owners.len().to_string()),
            label("   Participants "),
            Span::raw(party.participants.len().to_string()),
            label("   Contracts "),
            Span::raw(party.contracts.len().to_string()),
        ]),
    ];
    if let Some(key) = &party.my_owner_key {
        lines.push(Line::from(vec![
            label("Your key  "),
            Span::styled(key.clone(), Style::default().fg(logo::ORANGE)),
        ]));
    }
    match data.map(|d| &d.gov_state) {
        Some(Ok(Some(state))) => {
            let timeout = state
                .action_confirmation_timeout_microseconds
                .map_or_else(|| "—".to_owned(), format_micros_human);
            lines.push(Line::from(vec![
                label("Governance"),
                Span::raw(format!(
                    "  threshold {threshold} of {members} members · timeout {timeout}",
                    threshold = state.threshold,
                    members = state.members.len(),
                )),
            ]));
            if state.out_of_date {
                lines.push(Line::styled(
                    "⚠ governance-core out of date — migrate this party",
                    Style::default().fg(Color::Yellow),
                ));
            }
        }
        // Surface a load error on its own line, like holdings / audit do.
        Some(Err(error)) => lines.push(Line::from(vec![
            label("Governance"),
            Span::styled(
                format!("  {}", truncate(error, 60)),
                Style::default().fg(Color::Red),
            ),
        ])),
        _ => {}
    }
    let block = popup_block(&format!("Party · {}", party_name(party)));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The participants box: one row per participant with permission + owner tag.
fn draw_participants_box(frame: &mut Frame, area: Rect, party: &DecentralizedParty) {
    let block = popup_block("Participants");
    if party.participants.is_empty() {
        frame.render_widget(Paragraph::new(dim_line("(none)")).block(block), area);
        return;
    }
    let lines: Vec<Line> = party
        .participants
        .iter()
        .map(|participant| {
            let mut spans = vec![
                Span::raw(participant.participant_uid.to_string()),
                Span::raw("  "),
                Span::styled(
                    format!("[{}]", participant.permission),
                    Style::default().fg(permission_color(&participant.permission)),
                ),
            ];
            if participant.owner_key.is_some() {
                spans.push(Span::styled(" owner", Style::default().fg(logo::ORANGE)));
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The contracts box: package name/version and a truncated contract id.
fn draw_contracts_box(frame: &mut Frame, area: Rect, party: &DecentralizedParty) {
    let block = popup_block("Contracts");
    if party.contracts.is_empty() {
        frame.render_widget(Paragraph::new(dim_line("(none)")).block(block), area);
        return;
    }
    let lines: Vec<Line> = party
        .contracts
        .iter()
        .map(|contract| {
            let name = if contract.package_name.is_empty() {
                contract.template_id.clone()
            } else {
                format!("{} {}", contract.package_name, contract.package_version)
            };
            Line::from(vec![
                Span::raw(name),
                Span::raw("  "),
                Span::styled(
                    truncate(&contract.contract_id, 24),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The holdings box: asset / admin / amount / preapproval, or loading / error.
fn draw_holdings_box(frame: &mut Frame, area: Rect, data: Option<&DetailData>) {
    let block = popup_block("Holdings");
    let widget = match data.map(|data| &data.holdings) {
        None => Paragraph::new(dim_line("loading…")).block(block),
        Some(Err(error)) => {
            Paragraph::new(Line::styled(error.clone(), Style::default().fg(Color::Red)))
                .block(block)
        }
        Some(Ok(holdings)) if holdings.is_empty() => {
            Paragraph::new(dim_line("(none)")).block(block)
        }
        Some(Ok(holdings)) => {
            let mut lines = vec![Line::styled(
                format!("{:<8}{:<22}{:>16}  PREAPPROVAL", "ASSET", "ADMIN", "AMOUNT"),
                Style::default().fg(Color::DarkGray),
            )];
            lines.extend(holdings.iter().map(holding_line));
            Paragraph::new(lines).block(block)
        }
    };
    frame.render_widget(widget, area);
}

/// The audit box: a selectable table of entries (Enter opens the JSON modal).
fn draw_audit_box(
    frame: &mut Frame,
    area: Rect,
    data: Option<&DetailData>,
    state: &mut TableState,
) {
    let block = popup_block("Audit");
    match data.map(|data| &data.audit) {
        None => frame.render_widget(Paragraph::new(dim_line("loading…")).block(block), area),
        Some(Err(error)) => frame.render_widget(
            Paragraph::new(Line::styled(error.clone(), Style::default().fg(Color::Red)))
                .block(block),
            area,
        ),
        Some(Ok(entries)) if entries.is_empty() => {
            frame.render_widget(
                Paragraph::new(dim_line("(no audit entries)")).block(block),
                area,
            );
        }
        Some(Ok(entries)) => {
            let header = Row::new(["TIME", "ACTION", "TYPE", "STATUS"]).style(header_style());
            let rows = entries.iter().map(|entry| {
                Row::new(vec![
                    Cell::from(format_timestamp(entry.timestamp)),
                    Cell::from(audit_action(entry).to_owned()),
                    Cell::from(entry.governance_type.clone()),
                    Cell::from(Line::from(Span::styled(
                        entry.status.clone(),
                        Style::default().fg(audit_status_color(&entry.status)),
                    ))),
                ])
            });
            let widths = [
                Constraint::Length(17),
                Constraint::Fill(1),
                Constraint::Length(14),
                Constraint::Length(10),
            ];
            let table = Table::new(rows, widths)
                .header(header)
                .block(block)
                .column_spacing(2)
                .highlight_symbol("▶ ")
                .row_highlight_style(highlight_style());
            frame.render_stateful_widget(table, area, state);
        }
    }
}

/// One holdings row, aligned: asset, admin, amount, preapproval (+ locked note).
fn holding_line(holding: &Holding) -> Line<'_> {
    // Canton Coin's token-standard instrument id is "Amulet".
    let asset = if holding.instrument_id == "Amulet" {
        "CC"
    } else {
        &holding.instrument_id
    };
    let mut spans = vec![Span::raw(format!(
        "{}{}{}  ",
        fixed(asset, 8),
        fixed(&truncate_middle(&holding.instrument_admin, 20), 22),
        fixed_right(&holding.amount, 16),
    ))];
    let (label, color) = if holding.preapproval_set_up {
        ("yes", Color::Green)
    } else {
        ("no", Color::DarkGray)
    };
    spans.push(Span::styled(label, Style::default().fg(color)));
    if holding.locked_amount.parse::<f64>().is_ok_and(|v| v > 0.0) {
        spans.push(Span::styled(
            format!("  ({} locked)", holding.locked_amount),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use common::canton_id::CantonId;
    use common::types::{AuditLogEntry, ContractInfo, ParticipantInfo};

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use ratatui::widgets::TableState;

    use common::types::{DecentralizedParty, Permission};

    use crate::api::Holding;
    use crate::app::DetailData;

    use super::*;
    use crate::ui::fixtures::{NS2, canton_id};

    #[test]
    fn party_detail_renders_fields() {
        use crate::api::GovState;

        let party_id = canton_id("cbtc-network");
        let participant_id = CantonId::parse(&format!("participant-1::{NS2}")).unwrap();
        let party = DecentralizedParty {
            party_id: party_id.clone(),
            threshold: 2,
            confirmation_threshold: None,
            owners: vec!["a".to_owned()],
            my_owner_key: Some("1220deadbeef".to_owned()),
            participants: vec![ParticipantInfo {
                participant_uid: participant_id.clone(),
                permission: Permission::Submission,
                owner_key: None,
            }],
            non_hosting_owners: Vec::new(),
            contracts: vec![ContractInfo {
                contract_id: "00abc".to_owned(),
                template_id: "Splice:Rules".to_owned(),
                package_id: "1220cafe".to_owned(),
                package_name: "splice-amulet".to_owned(),
                package_version: "0.1.18".to_owned(),
                created_at: String::new(),
            }],
            local_metadata: None,
        };
        let data = DetailData {
            holdings: Ok(vec![Holding {
                instrument_admin: "DSO::1220aabb".to_owned(),
                instrument_id: "Amulet".to_owned(),
                amount: "1234.5".to_owned(),
                locked_amount: "10".to_owned(),
                preapproval_set_up: true,
            }]),
            audit: Ok(vec![AuditLogEntry {
                id: 1,
                timestamp: 1_750_000_000,
                event_type: "execute".to_owned(),
                party_id: party_id.clone(),
                member_party_id: party_id.clone(),
                governance_type: "core_self".to_owned(),
                action_summary: "Set threshold to 3".to_owned(),
                details: serde_json::json!({ "new_threshold": 3 }),
                status: "success".to_owned(),
                error_message: None,
                created_at: 1_750_000_000,
            }]),
            gov_state: Ok(Some(GovState {
                members: vec!["m1".to_owned(), "m2".to_owned(), "m3".to_owned()],
                threshold: 2,
                action_confirmation_timeout_microseconds: Some(86_400_000_000),
                out_of_date: false,
            })),
        };

        let mut audit_state = TableState::default().with_selected(Some(0));
        // Wide enough for a participant row to show the full Canton id (a
        // ~83-char `prefix::namespace`) AND the trailing `[permission]` tag,
        // which the boxes lay out full-width (vertical layout).
        let mut terminal = Terminal::new(TestBackend::new(120, 26)).unwrap();
        terminal
            .draw(|frame| {
                draw_party_detail(frame, frame.area(), &party, Some(&data), &mut audit_state)
            })
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        // Each section renders in its own framed box, with its data. The party
        // id and participant uid render as the full Canton id; assert on the
        // visible prefix segment.
        assert!(rendered.contains("cbtc-network::"));
        assert!(rendered.contains("Participants"));
        assert!(rendered.contains("participant-1::"));
        assert!(rendered.contains("submission"));
        assert!(rendered.contains("Contracts"));
        assert!(rendered.contains("splice-amulet"));
        assert!(rendered.contains("1220deadbeef"));
        assert!(rendered.contains("Holdings"));
        assert!(rendered.contains("ADMIN"));
        assert!(rendered.contains("CC"));
        assert!(rendered.contains("DSO::1220aabb"));
        assert!(rendered.contains("1234.5"));
        assert!(rendered.contains("locked"));
        // The audit table renders its header, the selected-row marker, and rows.
        assert!(rendered.contains("Audit"));
        assert!(rendered.contains("ACTION"));
        assert!(rendered.contains('▶'));
        assert!(rendered.contains("2025-"));
        assert!(rendered.contains("Set threshold to 3"));
        assert!(rendered.contains("success"));
        // The JSON is not shown inline — it opens as a modal (see below).
        assert!(!rendered.contains("new_threshold"));
        // The on-ledger governance state renders in the summary box (a
        // 86_400_000_000µs timeout renders as "24h", not "1d").
        assert!(rendered.contains("Governance"));
        assert!(rendered.contains("threshold 2 of 3 members"));
        assert!(rendered.contains("24h"));
    }

    #[test]
    fn format_micros_human_uses_hours_for_one_day_and_days_for_two() {
        assert_eq!(format_micros_human(86_400_000_000), "24h");
        assert_eq!(format_micros_human(172_800_000_000), "2d");
        assert_eq!(format_micros_human(3_600_000_000), "1h");
        assert_eq!(format_micros_human(1_800_000_000), "30m");
    }
}
