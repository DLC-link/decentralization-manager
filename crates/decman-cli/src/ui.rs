use std::collections::HashSet;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, Padding, Paragraph, Row, Table, TableState, Wrap,
};

use common::types::{
    ConnectionStatus, DecentralizedParty, PeerErrorKind, PeerPackageComparison, PendingInvitation,
    Permission, VettedPackageInfo, WorkflowProgress, WorkflowRun,
};

use crate::api::{
    AuthStatusKind, ChainAuditEntry, FeedItem, GovState, Holding, PartyAuthStatus, PeerView,
    audit_action, invitation_name, party_name, run_name,
};
use crate::app::{
    App, ComposerKind, ComposerPick, DeployForm, DetailData, GovItem, GovView, GrantForm, KickForm,
    NetworkEditState, OnboardForm, Overlay, PeerChoice, PeerForm, Status, Tab, TabView,
};
use crate::composer::{Composer, FieldKind};
use crate::config::Profile;
use crate::logo;

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

    draw_header(frame, header);

    let spinner = SPINNER[app.tick() % SPINNER.len()];

    // The party detail view replaces the tabs while open.
    if app.detail_open() {
        if let Some((party, data, audit_state)) = app.detail_view() {
            draw_party_detail(frame, body, party, data, audit_state);
        }
        let hint = match app.overlay() {
            Overlay::Json { .. } | Overlay::ChainAudit { .. } => " ↑/↓ scroll · esc close",
            Overlay::Kick(_) => " ↑/↓ pick · ←/→ threshold · enter kick · esc cancel",
            Overlay::Governance(_) => {
                " ↑/↓ · c confirm · e exec · r revoke · x expire · n new · p propose · esc"
            }
            Overlay::ComposerPick(_) => " ↑/↓ pick · enter open · esc cancel",
            Overlay::Composer(_) => {
                " ↑/↓ field · type · ←/→ toggle · enter next/submit · esc cancel"
            }
            Overlay::Deploy(_) => " ↑/↓ field · type number · enter next/deploy · esc cancel",
            Overlay::Message(_) | Overlay::Busy(_) => " enter / esc to close",
            _ => {
                " ↑/↓ audit · enter json · g gov · D deploy · c chain · K kick · esc back · q quit"
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

/// Draw the centered BitSafe wordmark and subtitle.
fn draw_header(frame: &mut Frame, area: Rect) {
    let mut lines = logo::lines();
    lines.push(Line::styled(
        SUBTITLE,
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

/// Render the login menu: a table of profiles to choose from.
pub fn draw_login(frame: &mut Frame, profiles: &[Profile], state: &mut TableState) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(7),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header);

    let header_row = Row::new(["PROFILE", "NETWORK", "USER", "API URL"]).style(header_style());
    let rows: Vec<Row> = profiles
        .iter()
        .map(|profile| {
            Row::new([
                profile.name.clone(),
                dash_if_empty(&profile.network),
                profile.username.clone(),
                profile.api_url.clone(),
            ])
        })
        .collect();
    let widths = [
        Constraint::Length(20),
        Constraint::Length(12),
        Constraint::Length(16),
        Constraint::Fill(1),
    ];
    let table = Table::new(rows, widths)
        .header(header_row)
        .block(popup_block("Select profile"))
        .column_spacing(2)
        .highlight_symbol("▶ ")
        .row_highlight_style(highlight_style());
    frame.render_stateful_widget(table, body, state);

    frame.render_widget(
        Paragraph::new(" ↑/↓ select · enter log in · q quit")
            .style(Style::default().fg(Color::DarkGray)),
        footer,
    );
}

/// The bordered panel for the active tab, with the tabs rendered as titles on
/// the top frame line — the active tab in brand orange, the others gray. An
/// optional search/filter hint is shown right-aligned on the same line.
fn tab_block(active: Tab, search: Option<(String, bool)>) -> Block<'static> {
    let mut title = vec![Span::raw(" ")];
    for (i, tab) in Tab::ALL.iter().enumerate() {
        if i > 0 {
            title.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        }
        let style = if *tab == active {
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        title.push(Span::styled(tab.title(), style));
    }
    title.push(Span::raw(" "));

    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(logo::ORANGE))
        .padding(Padding::horizontal(1))
        .title(Line::from(title));

    if let Some((text, active)) = search {
        let color = if active {
            logo::ORANGE
        } else {
            Color::DarkGray
        };
        block = block.title(
            Line::from(Span::styled(
                format!(" {text} "),
                Style::default().fg(color),
            ))
            .alignment(Alignment::Right),
        );
    }

    block
}

/// Draw the parties tab: a table of (filtered) parties or a placeholder.
fn draw_parties(
    frame: &mut Frame,
    area: Rect,
    status: &Status,
    parties: &[&DecentralizedParty],
    state: &mut TableState,
    block: Block<'_>,
    spinner: &str,
) {
    match status {
        Status::Loading => {
            frame.render_widget(loading(spinner, "Connecting to decman…", block), area)
        }
        Status::Error(message) => frame.render_widget(error_widget(message, block), area),
        Status::Loaded if parties.is_empty() => {
            frame.render_widget(placeholder("No decentralized parties found.", block), area);
        }
        Status::Loaded => frame.render_stateful_widget(parties_table(parties, block), area, state),
    }
}

/// Draw the peers tab: a table of network peers or a placeholder.
fn draw_peers(
    frame: &mut Frame,
    area: Rect,
    status: &Status,
    peers: &[PeerView],
    state: &mut TableState,
    block: Block<'_>,
    spinner: &str,
) {
    match status {
        Status::Loading => frame.render_widget(loading(spinner, "Probing peers…", block), area),
        Status::Error(message) => frame.render_widget(error_widget(message, block), area),
        Status::Loaded if peers.is_empty() => {
            frame.render_widget(placeholder("No peers configured.", block), area);
        }
        Status::Loaded => frame.render_stateful_widget(peers_table(peers, block), area, state),
    }
}

/// Draw the dars tab: a table of (filtered) vetted packages or a placeholder.
fn draw_dars(
    frame: &mut Frame,
    area: Rect,
    status: &Status,
    dars: &[&VettedPackageInfo],
    state: &mut TableState,
    block: Block<'_>,
    spinner: &str,
) {
    match status {
        Status::Loading => {
            frame.render_widget(loading(spinner, "Loading vetted packages…", block), area)
        }
        Status::Error(message) => frame.render_widget(error_widget(message, block), area),
        Status::Loaded if dars.is_empty() => {
            frame.render_widget(placeholder("No vetted packages found.", block), area);
        }
        Status::Loaded => frame.render_stateful_widget(dars_table(dars, block), area, state),
    }
}

/// Draw the workflows tab: pending invitations and workflow runs.
fn draw_feed(
    frame: &mut Frame,
    area: Rect,
    status: &Status,
    feed: &[FeedItem],
    state: &mut TableState,
    block: Block<'_>,
    spinner: &str,
) {
    match status {
        Status::Loading => frame.render_widget(loading(spinner, "Loading workflows…", block), area),
        Status::Error(message) => frame.render_widget(error_widget(message, block), area),
        Status::Loaded if feed.is_empty() => {
            frame.render_widget(placeholder("No workflows or invitations.", block), area);
        }
        Status::Loaded => frame.render_stateful_widget(feed_table(feed, block), area, state),
    }
}

/// Draw the party detail view: each section in its own framed box, with the
/// audit box a selectable table that fills the remaining space.
fn draw_party_detail(
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

/// The on-ledger governance state for the open party, if loaded.
fn gov_state_of(data: Option<&DetailData>) -> Option<&GovState> {
    data.and_then(|d| d.gov_state.as_ref().ok())
        .and_then(Option::as_ref)
}

/// Height of the summary box, accounting for the optional owner-key and
/// governance-state lines.
fn summary_height(party: &DecentralizedParty, data: Option<&DetailData>) -> u16 {
    let mut lines = 2u16;
    if party.my_owner_key.is_some() {
        lines += 1;
    }
    if let Some(state) = gov_state_of(data) {
        lines += 1;
        if state.out_of_date {
            lines += 1;
        }
    }
    lines + 2
}

/// Format a microsecond duration as a compact human string (e.g. `24h`, `30m`).
fn format_micros_human(micros: i64) -> String {
    let secs = micros / 1_000_000;
    if secs >= 86_400 && secs % 86_400 == 0 {
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
    if let Some(state) = gov_state_of(data) {
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

/// Syntax-highlighted, pretty-printed JSON lines for the audit-details modal.
fn json_lines(value: &serde_json::Value) -> Vec<Line<'static>> {
    if value.is_null() {
        return vec![Line::styled(
            "(no details)",
            Style::default().fg(Color::DarkGray),
        )];
    }
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| value.to_string())
        .lines()
        .map(|line| Line::from(highlight_json(line)))
        .collect()
}

/// Tokenize one line of (pretty-printed) JSON into color-styled spans: keys,
/// strings, numbers, `true`/`false`/`null`, and punctuation.
fn highlight_json(line: &str) -> Vec<Span<'static>> {
    let chars: Vec<char> = line.chars().collect();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            // String literal (handles escapes); a key if followed by `:`.
            let start = i;
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    '\\' => i += 2,
                    '"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            let text: String = chars[start..i.min(chars.len())].iter().collect();
            let mut j = i;
            while j < chars.len() && chars[j] == ' ' {
                j += 1;
            }
            let color = if chars.get(j) == Some(&':') {
                Color::Cyan
            } else {
                Color::Green
            };
            spans.push(Span::styled(text, Style::default().fg(color)));
        } else if c.is_ascii_digit()
            || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let start = i;
            i += 1;
            while i < chars.len() && matches!(chars[i], '0'..='9' | '.' | 'e' | 'E' | '+' | '-') {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            spans.push(Span::styled(text, Style::default().fg(Color::Yellow)));
        } else if let Some(literal) = json_literal_at(&chars, i) {
            spans.push(Span::styled(literal, Style::default().fg(Color::Magenta)));
            i += literal.len();
        } else {
            // A run of punctuation / whitespace up to the next token.
            let start = i;
            i += 1;
            while i < chars.len() && !json_token_starts(&chars, i) {
                i += 1;
            }
            let text: String = chars[start..i].iter().collect();
            spans.push(Span::styled(text, Style::default().fg(Color::DarkGray)));
        }
    }
    spans
}

/// The JSON keyword starting at `i`, if any.
fn json_literal_at(chars: &[char], i: usize) -> Option<&'static str> {
    ["true", "false", "null"].into_iter().find(|literal| {
        let end = i + literal.len();
        end <= chars.len() && chars[i..end].iter().copied().eq(literal.chars())
    })
}

/// Whether a JSON token (string, number, or keyword) begins at `i`.
fn json_token_starts(chars: &[char], i: usize) -> bool {
    let c = chars[i];
    c == '"'
        || c.is_ascii_digit()
        || (c == '-' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        || json_literal_at(chars, i).is_some()
}

/// A dim gray line of plain text.
fn dim_line(text: &str) -> Line<'_> {
    Line::styled(text, Style::default().fg(Color::DarkGray))
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

/// Pad/truncate `value` to exactly `width` columns, left-aligned.
fn fixed(value: &str, width: usize) -> String {
    if value.chars().count() > width {
        truncate(value, width)
    } else {
        format!("{value:<width$}")
    }
}

/// Right-align `value` to at least `width` columns.
fn fixed_right(value: &str, width: usize) -> String {
    format!("{value:>width$}")
}

/// Truncate keeping the head and tail with an ellipsis in the middle.
fn truncate_middle(value: &str, max: usize) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= max {
        return value.to_owned();
    }
    let tail = max.saturating_sub(1) / 2;
    let head = max.saturating_sub(1) - tail;
    let head: String = chars[..head].iter().collect();
    let tail: String = chars[chars.len() - tail..].iter().collect();
    format!("{head}…{tail}")
}

/// Color for an audit entry's status.
fn audit_status_color(status: &str) -> Color {
    match status {
        "success" | "completed" | "executed" => Color::Green,
        "failed" | "error" => Color::Red,
        _ => Color::DarkGray,
    }
}

/// Format an epoch timestamp (seconds or millis) as `YYYY-MM-DD HH:MM` (UTC).
fn format_timestamp(epoch: i64) -> String {
    if epoch <= 0 {
        return "—".to_owned();
    }
    let secs = if epoch > 1_000_000_000_000 {
        epoch / 1000
    } else {
        epoch
    };
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (hour, minute) = (rem / 3600, (rem % 3600) / 60);
    // Civil date from days since the Unix epoch (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// Color for a participant permission level.
fn permission_color(permission: &Permission) -> Color {
    match permission {
        Permission::Submission => Color::Green,
        Permission::Confirmation => logo::ORANGE,
        _ => Color::DarkGray,
    }
}

/// A bordered, centered single-line message (empty-state placeholders).
fn placeholder<'a>(message: &'a str, block: Block<'a>) -> Paragraph<'a> {
    Paragraph::new(message)
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center)
        .block(block)
}

/// A centered loading message with an animated spinner prefix.
fn loading<'a>(spinner: &str, message: &str, block: Block<'a>) -> Paragraph<'a> {
    Paragraph::new(format!("{spinner} {message}"))
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center)
        .block(block)
}

/// A bordered error panel with a retry hint.
fn error_widget<'a>(message: &'a str, block: Block<'a>) -> Paragraph<'a> {
    let text = vec![
        Line::styled(
            "Failed to reach decman",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Line::default(),
        Line::raw(message),
        Line::default(),
        Line::styled(
            "Press r to retry · q to quit",
            Style::default().fg(Color::DarkGray),
        ),
    ];
    Paragraph::new(text)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(block)
}

/// The orange selection-bar style shared by the tables.
fn highlight_style() -> Style {
    Style::default()
        .bg(logo::ORANGE)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

/// Style for a table header row.
fn header_style() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

/// Build the parties table with aligned columns and a header row.
fn parties_table<'a>(parties: &[&DecentralizedParty], block: Block<'a>) -> Table<'a> {
    let header = Row::new(["PARTY", "THRESHOLD", "OWNERS", "PARTICIPANTS"]).style(header_style());

    let rows: Vec<Row> = parties
        .iter()
        .map(|party| {
            Row::new([
                party_name(party).to_owned(),
                party.threshold.to_string(),
                party.owners.len().to_string(),
                party.participants.len().to_string(),
            ])
        })
        .collect();

    let widths = [
        Constraint::Fill(1),
        Constraint::Length(9),
        Constraint::Length(6),
        Constraint::Length(12),
    ];

    Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(2)
        .highlight_symbol("▶ ")
        .row_highlight_style(highlight_style())
}

/// Build the peers table, mirroring the frontend's network panel. The active
/// workflow (if any) is shown inline next to the peer name, as the frontend
/// does, so the name column keeps its width on narrow terminals.
fn peers_table<'a>(peers: &'a [PeerView], block: Block<'a>) -> Table<'a> {
    let header =
        Row::new(["STATUS", "PEER", "ADDRESS", "LATENCY", "VERSION"]).style(header_style());

    let rows = peers.iter().map(|peer| {
        let (color, label) = status_display(peer.status);
        let status = Cell::from(Line::from(vec![
            Span::styled("● ", Style::default().fg(color)),
            Span::styled(label, Style::default().fg(color)),
        ]));

        let mut name = vec![Span::raw(peer.name.clone())];
        if let Some(workflow) = &peer.workflow {
            name.push(Span::styled(
                format!("  ▸ {workflow}"),
                Style::default().fg(Color::DarkGray),
            ));
        }

        Row::new(vec![
            status,
            Cell::from(Line::from(name)),
            Cell::from(format!(
                "{addr}:{port}",
                addr = peer.address,
                port = peer.port
            )),
            Cell::from(
                peer.latency_ms
                    .map_or_else(|| "—".to_owned(), |ms| format!("{ms} ms")),
            ),
            Cell::from(peer.version.clone().unwrap_or_else(|| "—".to_owned())),
        ])
    });

    let widths = [
        Constraint::Length(14),
        Constraint::Fill(1),
        Constraint::Length(18),
        Constraint::Length(8),
        Constraint::Length(9),
    ];

    Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(2)
        .highlight_symbol("▶ ")
        .row_highlight_style(highlight_style())
}

/// Build the vetted-packages table: name, version, and a truncated package id.
fn dars_table<'a>(dars: &[&VettedPackageInfo], block: Block<'a>) -> Table<'a> {
    let header = Row::new(["PACKAGE", "VERSION", "PACKAGE ID"]).style(header_style());

    let rows: Vec<Row> = dars
        .iter()
        .map(|package| {
            Row::new([
                dash_if_empty(&package.package_name),
                dash_if_empty(&package.package_version),
                truncate(&package.package_id, 24),
            ])
        })
        .collect();

    let widths = [
        Constraint::Fill(1),
        Constraint::Length(14),
        Constraint::Length(25),
    ];

    Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(2)
        .highlight_symbol("▶ ")
        .row_highlight_style(highlight_style())
}

/// Build the feed table: pending invitations and workflow runs, with name in
/// its own column so it is shown in full. Invitations stand out with a cyan
/// `Invitation` status to signal they are actionable.
fn feed_table<'a>(feed: &[FeedItem], block: Block<'a>) -> Table<'a> {
    let header = Row::new(["WORKFLOW", "NAME", "STEP", "PROGRESS", "STATUS"]).style(header_style());

    let rows: Vec<Row> = feed
        .iter()
        .map(|item| match item {
            FeedItem::Invitation(invitation) => Row::new(vec![
                Cell::from(dash_if_empty(invitation.invitation_type.as_str())),
                Cell::from(invitation_name(invitation)),
                Cell::from("—".to_owned()),
                Cell::from("—".to_owned()),
                Cell::from(Line::from(Span::styled(
                    "Invitation",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))),
            ]),
            FeedItem::Run(run) => {
                let (color, label) = workflow_status_display(run.status);
                Row::new(vec![
                    Cell::from(dash_if_empty(run.kind.as_str())),
                    Cell::from(dash_if_empty(run_name(run))),
                    Cell::from(dash_if_empty(&run.current_step)),
                    Cell::from(format!("{}/{}", run.step_index, run.step_total)),
                    Cell::from(Line::from(Span::styled(label, Style::default().fg(color)))),
                ])
            }
        })
        .collect();

    let widths = [
        Constraint::Length(11),
        Constraint::Fill(1),
        Constraint::Length(16),
        Constraint::Length(8),
        Constraint::Length(13),
    ];

    Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(2)
        .highlight_symbol("▶ ")
        .row_highlight_style(highlight_style())
}

/// Map a workflow status to its display color and label.
fn workflow_status_display(status: WorkflowProgress) -> (Color, &'static str) {
    match status {
        WorkflowProgress::InProgress => (logo::ORANGE, "In progress"),
        WorkflowProgress::Completed => (Color::Green, "Completed"),
        WorkflowProgress::Failed => (Color::Red, "Failed"),
        WorkflowProgress::Cancelled => (Color::DarkGray, "Cancelled"),
        WorkflowProgress::Idle => (Color::DarkGray, "Idle"),
    }
}

/// Return the string, or an em dash when it is empty.
fn dash_if_empty(value: &str) -> String {
    if value.is_empty() {
        "—".to_owned()
    } else {
        value.to_owned()
    }
}

/// Truncate a string to `max` characters, appending an ellipsis when clipped.
fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_owned();
    }
    let kept: String = value.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// Snake_case label for a peer error kind, matching its wire representation —
/// the string the package-checker popup previously rendered verbatim.
fn peer_error_label(kind: PeerErrorKind) -> &'static str {
    match kind {
        PeerErrorKind::TcpConnectTimeout => "tcp_connect_timeout",
        PeerErrorKind::TcpConnectFailed => "tcp_connect_failed",
        PeerErrorKind::RequestTimeout => "request_timeout",
        PeerErrorKind::Transport => "transport",
        PeerErrorKind::HandshakeFailed => "handshake_failed",
        PeerErrorKind::BadStatus => "bad_status",
        PeerErrorKind::DecodeFailed => "decode_failed",
        PeerErrorKind::InvalidPublicKey => "invalid_public_key",
        PeerErrorKind::Other => "other",
    }
}

/// Map a peer status to its display color and label. `None` means no status was
/// reported for the peer, rendered as "Unknown".
fn status_display(status: Option<ConnectionStatus>) -> (Color, &'static str) {
    match status {
        Some(ConnectionStatus::CurrentNode) => (logo::ORANGE, "This node"),
        Some(ConnectionStatus::Connected) => (Color::Green, "Connected"),
        Some(ConnectionStatus::Unreachable) => (Color::Red, "Unreachable"),
        Some(ConnectionStatus::HandshakeFailed) => (Color::Red, "Handshake"),
        None => (Color::DarkGray, "Unknown"),
    }
}

/// Right-side footer summary for list tabs.
fn summary_line(status: &Status, count: usize, noun: &str) -> String {
    match status {
        Status::Loading => "loading… ".to_owned(),
        Status::Loaded => format!("{count} {noun} "),
        Status::Error(_) => "unreachable ".to_owned(),
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
        Overlay::Kick(_) => " ↑/↓ pick · ←/→ threshold · enter kick · esc cancel".to_owned(),
        Overlay::Auth { .. } => " ↑/↓ select · t test · g grant rights · esc close".to_owned(),
        Overlay::GrantRights(_) => " ↑/↓ field · type · enter next/grant · esc cancel".to_owned(),
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
        Overlay::Message(_) => " enter / esc to close".to_owned(),
        Overlay::Busy(_) => " working… · esc to dismiss".to_owned(),
        Overlay::None => {
            let base = match active {
                Tab::Parties => " ↑↓ nav · enter view · n onboard · r refresh",
                Tab::Peers => " ↑↓ nav · e edit · tab switch",
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

/// Compute a [`Rect`] of the given size centered within `area`, clamped to fit.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width - width) / 2;
    let y = area.y + (area.height - height) / 2;
    Rect::new(x, y, width, height)
}

/// The bordered block used by modal popups.
fn popup_block(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(logo::ORANGE))
        .padding(Padding::horizontal(1))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD),
        ))
}

/// Draw the active modal overlay (if any) over the rest of the UI.
fn draw_overlay(frame: &mut Frame, area: Rect, overlay: &Overlay, spinner: &str) {
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
        Overlay::Kick(form) => kick_popup(frame, area, form),
        Overlay::FeedDetail { item, scroll } => feed_detail_popup(frame, area, item, *scroll),
        Overlay::ChainAudit { entries, scroll } => chain_audit_popup(frame, area, entries, *scroll),
        Overlay::Auth { parties, selected } => auth_popup(frame, area, parties, *selected),
        Overlay::GrantRights(form) => grant_popup(frame, area, form),
        Overlay::Governance(view) => governance_popup(frame, area, view),
        Overlay::ComposerPick(pick) => composer_pick_popup(frame, area, pick),
        Overlay::Composer(composer) => composer_popup(frame, area, composer),
        Overlay::Deploy(form) => deploy_popup(frame, area, form),
        Overlay::NetworkEdit(state) => network_edit_popup(frame, area, state),
    }
}

/// The network-config editor popup: the peer list, or the add-peer sub-form.
fn network_edit_popup(frame: &mut Frame, area: Rect, state: &NetworkEditState) {
    match &state.adding {
        Some(form) => peer_form_popup(frame, area, form),
        None => peer_list_popup(frame, area, state),
    }
}

/// The peer list view of the network editor.
fn peer_list_popup(frame: &mut Frame, area: Rect, state: &NetworkEditState) {
    let mut lines: Vec<Line> = Vec::new();
    if state.peers.is_empty() {
        lines.push(dim_line("(no peers — press a to add)"));
    }
    for (index, peer) in state.peers.iter().enumerate() {
        let selected = index == state.selected;
        let marker = if selected { "▶ " } else { "  " };
        let style = if selected {
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{marker}{}", id_prefix(&peer.participant_id)),
                style,
            ),
            Span::styled(
                format!("  {}:{}", peer.address, peer.port),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    let width = 72.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16).saturating_add(2)).clamp(6, area.height.saturating_sub(4));
    let visible = height.saturating_sub(2);
    let selected = u16::try_from(state.selected).unwrap_or(0);
    let scroll = selected.saturating_sub(visible.saturating_sub(1));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .block(popup_block("Network peers"));
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The add-peer sub-form of the network editor.
fn peer_form_popup(frame: &mut Frame, area: Rect, form: &PeerForm) {
    let edit_line = |label: &str, value: &str, focused: bool| {
        let value_style = if focused {
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let shown = if focused {
            format!("{value}▏")
        } else if value.is_empty() {
            "(empty)".to_owned()
        } else {
            value.to_owned()
        };
        Line::from(vec![detail_label(label), Span::styled(shown, value_style)])
    };

    let fields = [
        ("Participant id", form.participant_id.as_str()),
        ("Name", form.name.as_str()),
        ("Address", form.address.as_str()),
        ("Port", form.port.as_str()),
        ("Public key", form.public_key.as_str()),
        ("Party (optional)", form.party.as_str()),
    ];
    let mut lines: Vec<Line> = fields
        .iter()
        .enumerate()
        .map(|(index, (label, value))| edit_line(label, value, form.cursor == index))
        .collect();
    lines.push(Line::default());
    let submit_style = if form.cursor >= fields.len() {
        Style::default()
            .fg(Color::Black)
            .bg(logo::ORANGE)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    lines.push(Line::from(Span::styled(" Add peer ", submit_style)));

    let width = 72.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16).saturating_add(2)).clamp(6, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines).block(popup_block("Add peer"));
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The governance-core contract deployment popup: resolved context plus the two
/// editable fields (threshold, timeout) and a submit row.
fn deploy_popup(frame: &mut Frame, area: Rect, form: &DeployForm) {
    let edit_line = |label: &str, value: &str, focused: bool| {
        let value_style = if focused {
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let shown = if focused {
            format!("{value}▏")
        } else {
            value.to_owned()
        };
        Line::from(vec![detail_label(label), Span::styled(shown, value_style)])
    };

    let mut lines = vec![
        detail_kv("Contract", "GovernanceRules".to_owned()),
        detail_kv(
            "Package",
            if form.package_id.is_empty() {
                "(governance-core DAR not vetted)".to_owned()
            } else {
                truncate(&form.package_id, 44)
            },
        ),
        detail_kv("Members", form.member_parties.len().to_string()),
    ];
    lines.extend(
        form.member_parties
            .iter()
            .map(|member| detail_item(id_prefix(member).to_owned())),
    );
    lines.push(Line::default());
    lines.push(edit_line("Threshold", &form.threshold, form.cursor == 0));
    lines.push(edit_line(
        "Timeout (µs)",
        &form.timeout_micros,
        form.cursor == 1,
    ));
    lines.push(Line::default());
    let submit_style = if form.cursor >= 2 {
        Style::default()
            .fg(Color::Black)
            .bg(logo::ORANGE)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    lines.push(Line::from(Span::styled(" Deploy ", submit_style)));

    let width = 70.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16).saturating_add(2)).clamp(6, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let title = format!("Deploy governance · {}", form.party_name);
    let paragraph = Paragraph::new(lines)
        .scroll((0, 0))
        .block(popup_block(&title));
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The action / proposal type picker popup.
fn composer_pick_popup(frame: &mut Frame, area: Rect, pick: &ComposerPick) {
    let title = match pick.kind {
        ComposerKind::Action => "New governance action",
        ComposerKind::Proposal => "New proposal",
    };
    let lines: Vec<Line> = pick
        .options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let selected = index == pick.selected;
            let marker = if selected { "▶ " } else { "  " };
            let style = if selected {
                Style::default()
                    .fg(logo::ORANGE)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(format!("{marker}{}", option.label), style))
        })
        .collect();

    let width = 56.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16).saturating_add(2)).clamp(6, area.height.saturating_sub(4));
    let visible = height.saturating_sub(2);
    let selected = u16::try_from(pick.selected).unwrap_or(0);
    let scroll = selected.saturating_sub(visible.saturating_sub(1));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .block(popup_block(title));
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The display string for a composer field's current value.
fn composer_field_display(field: &crate::composer::ComposerField, focused: bool) -> String {
    match &field.kind {
        FieldKind::Bool => format!("‹ {} ›", field.value),
        FieldKind::Select(options) => {
            let label = options
                .iter()
                .find(|option| option.value == field.value)
                .map_or(field.value.as_str(), |option| option.label);
            format!("‹ {label} ›")
        }
        FieldKind::List | FieldKind::Rows(_) => {
            let preview = field.value.replace('\n', " | ");
            if focused {
                format!("{preview}▏")
            } else if preview.is_empty() {
                "(empty)".to_owned()
            } else {
                preview
            }
        }
        FieldKind::Text | FieldKind::Int => {
            if focused {
                format!("{}▏", field.value)
            } else if field.value.is_empty() {
                "(empty)".to_owned()
            } else {
                field.value.clone()
            }
        }
    }
}

/// The composer form popup: one row per field plus a virtual submit row.
fn composer_popup(frame: &mut Frame, area: Rect, composer: &Composer) {
    let mut lines: Vec<Line> = Vec::new();
    for (index, field) in composer.fields.iter().enumerate() {
        let focused = index == composer.cursor;
        let value_style = if focused {
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans = vec![
            Span::styled(
                format!("{:<24}", truncate(field.label, 24)),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(composer_field_display(field, focused), value_style),
        ];
        if focused && !field.help.is_empty() {
            spans.push(Span::styled(
                format!("   {}", field.help),
                Style::default().fg(Color::DarkGray),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
    let submit_focused = composer.cursor >= composer.fields.len();
    let submit_style = if submit_focused {
        Style::default()
            .fg(Color::Black)
            .bg(logo::ORANGE)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    lines.push(Line::from(Span::styled(" Submit ", submit_style)));

    let width = 78.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16).saturating_add(2)).clamp(6, area.height.saturating_sub(4));
    let visible = height.saturating_sub(2);
    let cursor = u16::try_from(composer.cursor).unwrap_or(0);
    let scroll = cursor.saturating_sub(visible.saturating_sub(1));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .block(popup_block(&composer.title));
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// Title-case a snake_case identifier (e.g. `set_threshold` → `Set Threshold`).
fn humanize(value: &str) -> String {
    let mut out = String::new();
    for (i, word) in value.split('_').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// A short human summary of an off-chain governance action (its `type` plus a
/// salient field).
fn gov_action_summary(action: &serde_json::Value) -> String {
    let field_str = |key: &str| action.get(key).and_then(serde_json::Value::as_str);
    let field_i64 = |key: &str| action.get(key).and_then(serde_json::Value::as_i64);
    let kind = field_str("type").unwrap_or("action");
    let base = humanize(kind);
    let detail = match kind {
        "governance_set_threshold" => field_i64("new_threshold").map(|n| format!("→ {n}")),
        "governance_add_member" | "governance_remove_member" => {
            field_str("member").map(|m| id_prefix(m).to_owned())
        }
        "governance_set_timeout" => field_i64("new_timeout_microseconds").map(format_micros_human),
        "vault_pause" | "vault_unpause" | "vault_update_limits" | "vault_update_backend" => {
            field_str("vault_id").map(|v| truncate(v, 12))
        }
        _ => None,
    };
    match detail {
        Some(detail) => format!("{base}  {detail}"),
        None => base,
    }
}

/// The governance approvals popup: a selectable list of pending actions.
fn governance_popup(frame: &mut Frame, area: Rect, view: &GovView) {
    let ready = || Span::styled("  ✓ ready", Style::default().fg(Color::Green));
    let dim = || Style::default().fg(Color::DarkGray);

    let mut lines: Vec<Line> = Vec::new();
    if view.items.is_empty() {
        lines.push(dim_line("(no pending approvals)"));
    }
    for (index, item) in view.items.iter().enumerate() {
        let selected = index == view.selected;
        let marker = if selected { "▶ " } else { "  " };
        let style = if selected {
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let mut spans = vec![Span::styled(marker, style)];
        match item {
            GovItem::OffChain(action) => {
                spans.push(Span::styled(gov_action_summary(&action.action), style));
                spans.push(Span::styled(
                    format!("  {}/{}", action.confirmation_count, view.threshold),
                    dim(),
                ));
                if action.can_execute {
                    spans.push(ready());
                }
            }
            GovItem::Domain(domain) => {
                spans.push(Span::styled("[chain] ", Style::default().fg(Color::Cyan)));
                let label = match domain.description.as_deref().filter(|d| !d.is_empty()) {
                    Some(description) => format!("{} — {description}", domain.action_label),
                    None => domain.action_label.clone(),
                };
                spans.push(Span::styled(label, style));
                spans.push(Span::styled(
                    format!("  {}", domain.confirmation_count),
                    dim(),
                ));
                if domain.can_execute {
                    spans.push(ready());
                }
                if domain.orphaned {
                    spans.push(Span::styled("  orphaned", Style::default().fg(Color::Red)));
                }
            }
        }
        lines.push(Line::from(spans));
    }

    let width = 86.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16).saturating_add(2)).clamp(6, area.height.saturating_sub(4));
    let visible = height.saturating_sub(2);
    let selected = u16::try_from(view.selected).unwrap_or(0);
    let scroll = selected.saturating_sub(visible.saturating_sub(1));
    let rect = centered_rect(width, height, area);
    let title = format!(
        "Approvals · {party} · threshold {threshold}",
        party = view.party_name,
        threshold = view.threshold
    );
    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .block(popup_block(&title));
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The human-readable prefix of a Canton id (segment before `::`).
fn id_prefix(id: &str) -> &str {
    id.split("::").next().unwrap_or(id)
}

/// Display color and label for a per-party authentication status.
fn auth_status_display(status: &AuthStatusKind) -> (Color, String) {
    match status {
        AuthStatusKind::Authenticated => (Color::Green, "authenticated".to_owned()),
        AuthStatusKind::Mock => (logo::ORANGE, "mock".to_owned()),
        AuthStatusKind::Failed { error } => (Color::Red, format!("failed: {error}")),
        AuthStatusKind::Notconfigured => (Color::DarkGray, "not configured".to_owned()),
    }
}

/// A `✓ label` / `✗ label` chip, green when the right is held, else dim.
fn right_chip(label: &str, held: bool) -> Span<'static> {
    let (mark, color) = if held {
        ("✓", Color::Green)
    } else {
        ("✗", Color::DarkGray)
    };
    Span::styled(format!("{mark} {label}"), Style::default().fg(color))
}

/// The authentication-status popup: each party's IdP status and ledger rights.
fn auth_popup(frame: &mut Frame, area: Rect, parties: &[PartyAuthStatus], selected: usize) {
    let mut lines: Vec<Line> = Vec::new();
    if parties.is_empty() {
        lines.push(dim_line("(no parties configured)"));
    }
    let label = |text: &'static str| Span::styled(text, Style::default().fg(Color::DarkGray));
    // Line index where the selected party's block starts, for scroll-to-view.
    let mut selected_offset = 0u16;
    for (index, party) in parties.iter().enumerate() {
        if index == selected {
            selected_offset = u16::try_from(lines.len()).unwrap_or(0);
        }
        let marker = if index == selected { "▶ " } else { "  " };
        lines.push(Line::from(Span::styled(
            format!("{marker}{}", id_prefix(&party.dec_party_id)),
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD),
        )));
        if let Some(member) = &party.member_party_id {
            lines.push(Line::from(vec![
                label("  member  "),
                Span::raw(id_prefix(member).to_owned()),
            ]));
        }
        if let Some(user) = &party.user_id {
            lines.push(Line::from(vec![
                label("  user    "),
                Span::raw(user.clone()),
            ]));
        }
        let (color, status_label) = auth_status_display(&party.status);
        lines.push(Line::from(vec![
            label("  status  "),
            Span::styled(status_label, Style::default().fg(color)),
        ]));
        if let Some(rights) = &party.rights {
            lines.push(Line::from(vec![
                label("  rights  "),
                label("member "),
                right_chip("actAs", rights.member_party_act_as),
                Span::raw(" "),
                right_chip("readAs", rights.member_party_read_as),
                label("  · dec "),
                right_chip("actAs", rights.dec_party_act_as),
                Span::raw(" "),
                right_chip("readAs", rights.dec_party_read_as),
            ]));
        }
        lines.push(Line::default());
    }

    let width = 76.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16).saturating_add(2)).clamp(6, area.height.saturating_sub(4));
    let visible = height.saturating_sub(2);
    // Scroll so the selected party's block stays in view.
    let scroll = selected_offset.saturating_sub(visible.saturating_sub(1));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .block(popup_block("Authentication"));
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The grant-rights form popup: admin client id + secret for one party.
fn grant_popup(frame: &mut Frame, area: Rect, form: &GrantForm) {
    let edit_line = |label: &str, value: &str, focused: bool, mask: bool| {
        let value_style = if focused {
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let base = if mask {
            "•".repeat(value.chars().count())
        } else {
            value.to_owned()
        };
        let shown = if focused {
            format!("{base}▏")
        } else if value.is_empty() {
            "(empty)".to_owned()
        } else {
            base
        };
        Line::from(vec![detail_label(label), Span::styled(shown, value_style)])
    };

    let mut lines = vec![
        detail_kv("Party", form.party_name.clone()),
        Line::default(),
        edit_line("Admin client id", &form.client_id, form.cursor == 0, false),
        edit_line("Admin secret", &form.client_secret, form.cursor == 1, true),
        Line::default(),
    ];
    let submit_style = if form.cursor >= 2 {
        Style::default()
            .fg(Color::Black)
            .bg(logo::ORANGE)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    lines.push(Line::from(Span::styled(" Grant rights ", submit_style)));

    let width = 64.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16).saturating_add(2)).clamp(6, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines).block(popup_block("Grant rights"));
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// A scrollable popup of the on-chain governance audit trail.
fn chain_audit_popup(frame: &mut Frame, area: Rect, entries: &[ChainAuditEntry], scroll: u16) {
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
fn json_popup(frame: &mut Frame, area: Rect, value: &serde_json::Value, scroll: u16) {
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
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// A small centered popup with a wrapped message.
fn message_popup(frame: &mut Frame, area: Rect, title: &str, message: &str, color: Color) {
    let width = (message.chars().count() as u16 + 6).clamp(28, area.width.saturating_sub(6));
    let rect = centered_rect(width, 7, area);
    let paragraph = Paragraph::new(message.to_owned())
        .style(Style::default().fg(color))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(popup_block(title));
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The package-checker popup: local count and each peer's sync state.
fn compare_popup(frame: &mut Frame, area: Rect, comparison: &PeerPackageComparison, scroll: u16) {
    let local_ids: HashSet<&str> = comparison
        .local_packages
        .iter()
        .map(|package| package.package_id.as_str())
        .collect();

    let mut lines = vec![
        Line::from(Span::styled(
            format!("Local packages: {}", comparison.local_packages.len()),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::default(),
    ];
    for peer in &comparison.peers {
        let line = if peer.reachable {
            let peer_ids: HashSet<&str> = peer
                .packages
                .iter()
                .map(|p| p.package_id.as_str())
                .collect();
            let missing = local_ids
                .iter()
                .filter(|id| !peer_ids.contains(*id))
                .count();
            let (color, note) = if missing == 0 {
                (Color::Green, "in sync".to_owned())
            } else {
                (Color::Yellow, format!("{missing} missing"))
            };
            Line::from(vec![
                Span::styled("● ", Style::default().fg(color)),
                Span::raw(format!("{}  ", peer.name)),
                Span::styled(note, Style::default().fg(color)),
                Span::styled(
                    format!("  ({} pkgs)", peer.packages.len()),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        } else {
            let error = peer.error_kind.map_or("unreachable", peer_error_label);
            Line::from(vec![
                Span::styled("● ", Style::default().fg(Color::Red)),
                Span::raw(format!("{}  ", peer.name)),
                Span::styled(
                    format!("unreachable ({error})"),
                    Style::default().fg(Color::Red),
                ),
            ])
        };
        lines.push(line);
    }

    let width = 70.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16) + 2)
        .min(area.height.saturating_sub(4))
        .max(6);
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines)
        .scroll((scroll, 0))
        .block(popup_block("Package check"));
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The DAR distribution popup: a tickable list of peers.
fn peer_select_popup(
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
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The onboarding (create-party) form popup: a prefix field above a tickable
/// peer list, with the focused element highlighted.
fn onboard_popup(frame: &mut Frame, area: Rect, form: &OnboardForm) {
    let prefix_focus = form.cursor == 0;
    let prefix_span = if form.prefix.is_empty() && !prefix_focus {
        Span::styled("<party id prefix>", Style::default().fg(Color::DarkGray))
    } else {
        let cursor = if prefix_focus { "▏" } else { "" };
        Span::styled(
            format!("{}{cursor}", form.prefix),
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD),
        )
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Prefix  ", Style::default().fg(Color::DarkGray)),
            prefix_span,
        ]),
        Line::default(),
        dim_line("Invite peers:"),
    ];
    for (i, peer) in form.peers.iter().enumerate() {
        let focused = form.cursor == i + 1;
        let marker = if focused { "▶ " } else { "  " };
        let check = if peer.checked { "[x] " } else { "[ ] " };
        let style = if focused {
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{check}{}", peer.name),
            style,
        )));
    }

    let width = 64.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16) + 2).clamp(8, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines).block(popup_block("Onboard new party"));
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The kick-participant form popup: pick an owner to remove and set the new
/// signing threshold.
fn kick_popup(frame: &mut Frame, area: Rect, form: &KickForm) {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Party  ", Style::default().fg(Color::DarkGray)),
            Span::raw(form.party_name.clone()),
        ]),
        Line::default(),
        dim_line("Remove participant:"),
    ];
    for (i, candidate) in form.candidates.iter().enumerate() {
        let focused = i == form.selected;
        let marker = if focused { "▶ " } else { "  " };
        let style = if focused {
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{}", candidate.label),
            style,
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("New threshold  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{} ", form.new_threshold),
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "(was {prev}, max {max}) ←/→ adjust",
                prev = form.previous_threshold,
                max = form.max_threshold
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    let width = 64.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16) + 2).clamp(8, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines).block(popup_block("Kick participant"));
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// A scrollable detail popup for a feed item (workflow run or invitation).
fn feed_detail_popup(frame: &mut Frame, area: Rect, item: &FeedItem, scroll: u16) {
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
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// A right-padded dim field label for the detail popups.
fn detail_label(text: &str) -> Span<'static> {
    Span::styled(format!("{text:<15}"), Style::default().fg(Color::DarkGray))
}

/// A `label  value` detail line.
fn detail_kv(label: &str, value: String) -> Line<'static> {
    Line::from(vec![detail_label(label), Span::raw(value)])
}

/// An indented detail value line (for list items).
fn detail_item(value: String) -> Line<'static> {
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
    if !run.completed_peers.is_empty() || !run.expected_peers.is_empty() {
        lines.push(detail_kv(
            "Peers",
            format!(
                "{done}/{total} done",
                done = run.completed_peers.len(),
                total = run.expected_peers.len()
            ),
        ));
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
        lines.push(detail_kv("DARs", invitation.dar_filenames.join(", ")));
    }
    lines
}

#[cfg(test)]
mod tests {
    use common::canton_id::CantonId;
    use common::types::{
        AuditLogEntry, ContractInfo, InvitationType, ParticipantInfo, PendingInvitation,
        WorkflowKind, WorkflowRole, WorkflowRun,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    /// A valid 34-byte (68 hex char) namespace for building realistic Canton ids.
    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    /// A second valid namespace, distinct from [`NS`], for participant ids.
    const NS2: &str = "1220034c3a6a945442fb9f9b3f8e6a3f5e8c7d6b5a4938271605f4e3d2c1b0a99887";

    fn canton_id(prefix: &str) -> CantonId {
        CantonId::parse(&format!("{prefix}::{NS}")).unwrap()
    }

    fn render(mut body: impl FnMut(&mut Frame, Rect)) -> String {
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

    fn render_parties(parties: &[DecentralizedParty]) -> String {
        let refs: Vec<&DecentralizedParty> = parties.iter().collect();
        let mut state = TableState::default();
        render(|frame, area| {
            draw_parties(
                frame,
                area,
                &Status::Loaded,
                &refs,
                &mut state,
                tab_block(Tab::Parties, None),
                "⠋",
            );
        })
    }

    fn render_peers(peers: &[PeerView]) -> String {
        let mut state = TableState::default();
        render(|frame, area| {
            draw_peers(
                frame,
                area,
                &Status::Loaded,
                peers,
                &mut state,
                tab_block(Tab::Peers, None),
                "⠋",
            );
        })
    }

    fn render_dars(dars: &[VettedPackageInfo]) -> String {
        let refs: Vec<&VettedPackageInfo> = dars.iter().collect();
        let mut state = TableState::default();
        render(|frame, area| {
            draw_dars(
                frame,
                area,
                &Status::Loaded,
                &refs,
                &mut state,
                tab_block(Tab::Dars, None),
                "⠋",
            );
        })
    }

    fn sample_party() -> DecentralizedParty {
        DecentralizedParty {
            party_id: canton_id("cbtc-network"),
            threshold: 2,
            owners: vec!["a".to_owned(), "b".to_owned()],
            my_owner_key: None,
            participants: Vec::new(),
            contracts: Vec::new(),
            local_metadata: None,
        }
    }

    fn sample_peer() -> PeerView {
        PeerView {
            participant_id: "alpha::1220".to_owned(),
            name: "alpha".to_owned(),
            address: "10.0.0.1".to_owned(),
            port: 9001,
            status: Some(ConnectionStatus::Connected),
            latency_ms: Some(12),
            version: Some("1.2.3".to_owned()),
            workflow: None,
            is_self: false,
        }
    }

    #[test]
    fn tab_titles_render_on_the_frame() {
        let rendered = render_parties(&[sample_party()]);
        assert!(rendered.contains("Parties"));
        assert!(rendered.contains("Peers"));
        assert!(rendered.contains("Dars"));
        assert!(rendered.contains("Workflows"));
    }

    #[test]
    fn search_hint_renders_on_the_frame() {
        let refs: Vec<&DecentralizedParty> = Vec::new();
        let mut state = TableState::default();
        let rendered = render(|frame, area| {
            draw_parties(
                frame,
                area,
                &Status::Loaded,
                &refs,
                &mut state,
                tab_block(Tab::Parties, Some(("search: vault▏".to_owned(), true))),
                "⠋",
            );
        });
        assert!(rendered.contains("search: vault"));
    }

    #[test]
    fn parties_table_renders_headers_and_rows() {
        let rendered = render_parties(&[sample_party()]);
        assert!(rendered.contains("cbtc-network"));
        assert!(rendered.contains("PARTY"));
        assert!(rendered.contains("PARTICIPANTS"));
    }

    #[test]
    fn peers_table_renders_status_and_columns() {
        let rendered = render_peers(&[sample_peer()]);
        assert!(rendered.contains("alpha"));
        assert!(rendered.contains("Connected"));
        assert!(rendered.contains("10.0.0.1:9001"));
        assert!(rendered.contains("12 ms"));
    }

    #[test]
    fn dars_table_renders_headers_and_rows() {
        let dars = [VettedPackageInfo {
            package_id: "1220deadbeef".to_owned(),
            package_name: "splice-amulet".to_owned(),
            package_version: "1.2.3".to_owned(),
        }];
        let rendered = render_dars(&dars);
        assert!(rendered.contains("PACKAGE"));
        assert!(rendered.contains("splice-amulet"));
        assert!(rendered.contains("1.2.3"));
    }

    #[test]
    fn feed_renders_runs_and_invitations() {
        let feed = [
            FeedItem::Run(WorkflowRun {
                instance_name: "contracts-pending-xyz".to_owned(),
                kind: WorkflowKind::Contracts,
                role: WorkflowRole::Coordinator,
                status: WorkflowProgress::InProgress,
                current_step: "PrepareSubmissions".to_owned(),
                step_index: 2,
                step_total: 3,
                config_json: String::new(),
                coordinator_pubkey: None,
                coordinator_name: None,
                expected_peers: Vec::new(),
                completed_peers: Vec::new(),
                dec_party_id: None,
                prefix: None,
                participants: Vec::new(),
                previous_threshold: None,
                new_threshold: None,
                kicked_participant: None,
                package_names: Vec::new(),
                dar_filenames: Vec::new(),
                error: None,
                dismissed: false,
                created_at: 0,
                updated_at: 0,
            }),
            FeedItem::Invitation(PendingInvitation {
                id: "inv-1".to_owned(),
                invitation_type: InvitationType::Onboarding,
                coordinator_pubkey: "1220deadbeef".to_owned(),
                coordinator_name: Some("alice".to_owned()),
                received_at: 0,
                prefix: Some("vault-rc5".to_owned()),
                participants: Vec::new(),
                dar_filenames: Vec::new(),
                kicked_participant: None,
                new_threshold: None,
                previous_threshold: None,
                dec_party_id: None,
                package_names: Vec::new(),
                workflow_instance: None,
            }),
        ];
        let mut state = TableState::default();
        let rendered = render(|frame, area| {
            draw_feed(
                frame,
                area,
                &Status::Loaded,
                &feed,
                &mut state,
                tab_block(Tab::Workflows, None),
                "⠋",
            );
        });
        assert!(rendered.contains("Contracts"));
        // The full instance name is shown (not clipped to two characters).
        assert!(rendered.contains("contracts-pending-xyz"));
        assert!(rendered.contains("2/3"));
        assert!(rendered.contains("In progress"));
        // The invitation row is present and actionable.
        assert!(rendered.contains("Onboarding"));
        assert!(rendered.contains("vault-rc5"));
        assert!(rendered.contains("Invitation"));
    }

    #[test]
    fn party_detail_renders_fields() {
        let party_id = canton_id("cbtc-network");
        let participant_id = CantonId::parse(&format!("participant-1::{NS2}")).unwrap();
        let party = DecentralizedParty {
            party_id: party_id.clone(),
            threshold: 2,
            owners: vec!["a".to_owned()],
            my_owner_key: Some("1220deadbeef".to_owned()),
            participants: vec![ParticipantInfo {
                participant_uid: participant_id.clone(),
                permission: Permission::Submission,
                owner_key: None,
            }],
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
        // 86_400_000_000µs timeout renders as "1d").
        assert!(rendered.contains("Governance"));
        assert!(rendered.contains("threshold 2 of 3 members"));
        assert!(rendered.contains("1d"));
    }

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
    fn highlight_json_colors_tokens() {
        let spans = highlight_json(r#"  "key": "val", "count": 3, "ok": true, "x": null"#);
        let color_of = |text: &str| {
            spans
                .iter()
                .find(|span| span.content == text)
                .unwrap_or_else(|| panic!("no span {text:?}"))
                .style
                .fg
        };
        assert_eq!(color_of("\"key\""), Some(Color::Cyan)); // key
        assert_eq!(color_of("\"val\""), Some(Color::Green)); // string value
        assert_eq!(color_of("3"), Some(Color::Yellow)); // number
        assert_eq!(color_of("true"), Some(Color::Magenta)); // boolean
        assert_eq!(color_of("null"), Some(Color::Magenta)); // null
    }

    #[test]
    fn onboard_popup_renders_prefix_and_peers() {
        let overlay = Overlay::Onboard(OnboardForm {
            prefix: "vault".to_owned(),
            peers: vec![PeerChoice {
                id: "p1::1220".to_owned(),
                name: "alpha".to_owned(),
                checked: true,
            }],
            cursor: 0,
        });
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Onboard new party"));
        assert!(rendered.contains("vault"));
        assert!(rendered.contains("alpha"));
        assert!(rendered.contains("[x]"));
    }

    #[test]
    fn kick_popup_renders_candidates_and_threshold() {
        use crate::app::KickCandidate;

        let overlay = Overlay::Kick(KickForm {
            party_id: "dec::1220".to_owned(),
            party_name: "cbtc-network".to_owned(),
            previous_threshold: 3,
            candidates: vec![KickCandidate {
                participant_id: "p::1220".to_owned(),
                label: "peerA".to_owned(),
            }],
            selected: 0,
            new_threshold: 2,
            max_threshold: 2,
        });
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Kick participant"));
        assert!(rendered.contains("cbtc-network"));
        assert!(rendered.contains("peerA"));
        assert!(rendered.contains("New threshold"));
    }

    #[test]
    fn feed_detail_popup_renders_run_fields_and_error() {
        let run = WorkflowRun {
            instance_name: "onboarding-vault-abc".to_owned(),
            kind: WorkflowKind::Onboarding,
            role: WorkflowRole::Coordinator,
            status: WorkflowProgress::Failed,
            current_step: "WaitingForPeers".to_owned(),
            step_index: 1,
            step_total: 7,
            config_json: String::new(),
            coordinator_pubkey: None,
            coordinator_name: None,
            expected_peers: Vec::new(),
            completed_peers: Vec::new(),
            dec_party_id: None,
            prefix: Some("vault".to_owned()),
            participants: Vec::new(),
            previous_threshold: None,
            new_threshold: None,
            kicked_participant: None,
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

    #[test]
    fn auth_popup_renders_status_and_rights() {
        use crate::api::AuthRights;

        let overlay = Overlay::Auth {
            parties: vec![PartyAuthStatus {
                dec_party_id: "cbtc-network::1220".to_owned(),
                member_party_id: Some("member-a::1220".to_owned()),
                user_id: Some("user-1".to_owned()),
                status: AuthStatusKind::Authenticated,
                rights: Some(AuthRights {
                    member_party_act_as: true,
                    member_party_read_as: true,
                    dec_party_act_as: false,
                    dec_party_read_as: true,
                }),
            }],
            selected: 0,
        };
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Authentication"));
        assert!(rendered.contains("cbtc-network"));
        assert!(rendered.contains("member-a"));
        assert!(rendered.contains("user-1"));
        assert!(rendered.contains("authenticated"));
        assert!(rendered.contains("actAs"));
    }

    #[test]
    fn grant_popup_masks_secret() {
        let form = GrantForm {
            dec_party_id: "cbtc-network::1220".to_owned(),
            party_name: "cbtc-network".to_owned(),
            client_id: "validator-admin".to_owned(),
            client_secret: "supersecret".to_owned(),
            cursor: 0,
        };
        let overlay = Overlay::GrantRights(Box::new(form));
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Grant rights"));
        assert!(rendered.contains("validator-admin"));
        // The secret is masked, never shown verbatim.
        assert!(!rendered.contains("supersecret"));
        assert!(rendered.contains('•'));
    }

    #[test]
    fn gov_action_summary_humanizes_with_detail() {
        assert_eq!(
            gov_action_summary(
                &serde_json::json!({ "type": "governance_set_threshold", "new_threshold": 3 })
            ),
            "Governance Set Threshold  → 3"
        );
        assert_eq!(
            gov_action_summary(
                &serde_json::json!({ "type": "vault_pause", "vault_id": "00abcdef0123456789" })
            ),
            "Vault Pause  00abcdef012…"
        );
        assert_eq!(
            gov_action_summary(&serde_json::json!({ "type": "vault_deployment" })),
            "Vault Deployment"
        );
    }

    #[test]
    fn governance_popup_renders_offchain_and_domain() {
        use crate::api::{DomainGovAction, GovAction};

        let view = GovView {
            party_name: "cbtc-network".to_owned(),
            party_id: "dec::1220".to_owned(),
            governance_type: "vault".to_owned(),
            rules_contract_id: "00rules".to_owned(),
            member_party_id: "member::1220".to_owned(),
            threshold: 2,
            items: vec![
                GovItem::OffChain(GovAction {
                    action: serde_json::json!({
                        "type": "governance_set_threshold",
                        "new_threshold": 3,
                    }),
                    confirmations: Vec::new(),
                    confirmation_count: 1,
                    can_execute: false,
                }),
                GovItem::Domain(DomainGovAction {
                    proposal_cid: "00prop".to_owned(),
                    action_label: "SetupCcPreapproval".to_owned(),
                    description: Some("set up preapproval".to_owned()),
                    confirmations: Vec::new(),
                    confirmation_count: 2,
                    can_execute: true,
                    orphaned: false,
                }),
            ],
            selected: 0,
        };
        let overlay = Overlay::Governance(Box::new(view));
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Approvals"));
        assert!(rendered.contains("Governance Set Threshold"));
        assert!(rendered.contains("1/2"));
        assert!(rendered.contains("[chain]"));
        assert!(rendered.contains("SetupCcPreapproval"));
        assert!(rendered.contains("ready"));
    }

    #[test]
    fn composer_pick_popup_lists_options() {
        use crate::app::{ComposerKind, ComposerPick};

        let pick = ComposerPick {
            kind: ComposerKind::Action,
            party_id: "dec::1".to_owned(),
            party_name: "dec".to_owned(),
            governance_type: "core_self".to_owned(),
            rules_contract_id: "00r".to_owned(),
            default_threshold: 2,
            options: crate::composer::action_types("core_self"),
            selected: 0,
        };
        let overlay = Overlay::ComposerPick(Box::new(pick));
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("New governance action"));
        assert!(rendered.contains("Add governance member"));
    }

    #[test]
    fn composer_popup_renders_fields_and_submit() {
        use crate::composer::{ComposerContext, ComposerSubmit};

        let ctx = ComposerContext {
            party_id: "dec::1".to_owned(),
            operator_party: String::new(),
            default_threshold: 2,
        };
        let composer = Composer {
            title: "Set governance threshold".to_owned(),
            action_type: "governance_set_threshold",
            submit: ComposerSubmit::Confirm,
            party_id: "dec::1".to_owned(),
            party_name: "dec".to_owned(),
            governance_type: "core_self".to_owned(),
            rules_contract_id: "00r".to_owned(),
            fields: crate::composer::fields_for_action("governance_set_threshold", &ctx),
            cursor: 0,
        };
        let overlay = Overlay::Composer(Box::new(composer));
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Set governance threshold"));
        assert!(rendered.contains("New threshold"));
        assert!(rendered.contains("Submit"));
    }

    #[test]
    fn deploy_popup_renders_context_and_fields() {
        let form = DeployForm {
            party_id: "dec::1220".to_owned(),
            party_name: "cbtc-network".to_owned(),
            package_id: "#governance-core-v1".to_owned(),
            operator_party: "op::1220".to_owned(),
            participant_ids: vec!["p1::1220".to_owned()],
            member_parties: vec!["member-a::1220".to_owned()],
            threshold: "2".to_owned(),
            timeout_micros: "86400000000".to_owned(),
            cursor: 0,
        };
        let overlay = Overlay::Deploy(Box::new(form));
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Deploy governance"));
        assert!(rendered.contains("GovernanceRules"));
        assert!(rendered.contains("member-a"));
        assert!(rendered.contains("Threshold"));
        assert!(rendered.contains("Deploy"));
    }

    #[test]
    fn network_edit_popup_lists_peers() {
        use crate::api::PeerEntry;

        let state = NetworkEditState {
            peers: vec![PeerEntry {
                participant_id: "alpha::1220".to_owned(),
                name: "alpha".to_owned(),
                address: "10.0.0.1".to_owned(),
                port: 9001,
                public_key: "abcd".to_owned(),
                party: None,
            }],
            selected: 0,
            adding: None,
        };
        let overlay = Overlay::NetworkEdit(Box::new(state));
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Network peers"));
        assert!(rendered.contains("alpha"));
        assert!(rendered.contains("10.0.0.1:9001"));
    }

    #[test]
    fn network_edit_add_form_renders_fields() {
        let state = NetworkEditState {
            peers: Vec::new(),
            selected: 0,
            adding: Some(PeerForm {
                participant_id: String::new(),
                name: String::new(),
                address: String::new(),
                port: String::new(),
                public_key: String::new(),
                party: String::new(),
                cursor: 0,
            }),
        };
        let overlay = Overlay::NetworkEdit(Box::new(state));
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Add peer"));
        assert!(rendered.contains("Participant id"));
        assert!(rendered.contains("Public key"));
    }

    #[test]
    fn empty_feed_shows_placeholder() {
        let mut state = TableState::default();
        let rendered = render(|frame, area| {
            draw_feed(
                frame,
                area,
                &Status::Loaded,
                &[],
                &mut state,
                tab_block(Tab::Workflows, None),
                "⠋",
            );
        });
        assert!(rendered.contains("No workflows or invitations."));
    }
}
