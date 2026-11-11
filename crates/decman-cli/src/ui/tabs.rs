//! The four tab panels and their tables.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Padding, Row, Table, TableState};

use common::types::WorkflowProgress;
use common::types::{ConnectionStatus, DecentralizedParty, PeerErrorKind, VettedPackageInfo};

use crate::api::{FeedItem, PeerView, invitation_name, party_name, run_name};
use crate::app::{Status, Tab};
use crate::logo;

use crate::ui::widgets::{dash_if_empty, error_widget, header_style, highlight_style, loading};
use crate::ui::widgets::{placeholder, truncate};

/// The bordered panel for the active tab, with the tabs rendered as titles on
/// the top frame line — the active tab in brand orange, the others gray. An
/// optional search/filter hint is shown right-aligned on the same line.
pub fn tab_block(active: Tab, search: Option<(String, bool)>) -> Block<'static> {
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
pub fn draw_parties(
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
pub fn draw_peers(
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
pub fn draw_dars(
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
pub fn draw_feed(
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

    // ADDRESS flexes to fill the width (host:port strings are long); the peer
    // name column is fixed and comfortably fits a name plus its workflow note.
    let widths = [
        Constraint::Length(14),
        Constraint::Length(30),
        Constraint::Fill(1),
        Constraint::Length(9),
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
                // Only an in-progress run has a meaningful position; show it
                // 1-based ("step 3 of 8"), matching the web frontend. A finished
                // run's status column already says Completed / Failed.
                let progress = if run.status == WorkflowProgress::InProgress && run.step_total > 0 {
                    format!("{}/{}", run.step_index + 1, run.step_total)
                } else {
                    "—".to_owned()
                };
                Row::new(vec![
                    Cell::from(dash_if_empty(run.kind.as_str())),
                    Cell::from(dash_if_empty(run_name(run))),
                    Cell::from(dash_if_empty(&run.current_step)),
                    Cell::from(progress),
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
pub fn workflow_status_display(status: WorkflowProgress) -> (Color, &'static str) {
    match status {
        WorkflowProgress::InProgress => (logo::ORANGE, "In progress"),
        WorkflowProgress::Completed => (Color::Green, "Completed"),
        WorkflowProgress::Failed => (Color::Red, "Failed"),
        WorkflowProgress::Cancelled => (Color::DarkGray, "Cancelled"),
        WorkflowProgress::Idle => (Color::DarkGray, "Idle"),
    }
}

/// Snake_case label for a peer error kind, matching its wire representation —
/// the string the package-checker popup previously rendered verbatim.
pub fn peer_error_label(kind: PeerErrorKind) -> &'static str {
    match kind {
        PeerErrorKind::TopologyReadFailed => "topology_read_failed",
        PeerErrorKind::NoVettedPackages => "no_vetted_packages",
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
pub fn summary_line(status: &Status, count: usize, noun: &str) -> String {
    match status {
        Status::Loading => "loading… ".to_owned(),
        Status::Loaded => format!("{count} {noun} "),
        Status::Error(_) => "unreachable ".to_owned(),
    }
}

#[cfg(test)]
mod tests {

    use common::types::{InvitationType, WorkflowKind};

    use ratatui::widgets::TableState;

    use common::types::{ConnectionStatus, DecentralizedParty, PendingInvitation};
    use common::types::{VettedPackageInfo, WorkflowProgress, WorkflowRole, WorkflowRun};

    use crate::api::{FeedItem, PeerView};
    use crate::app::{Status, Tab};

    use super::*;
    use crate::ui::fixtures::{canton_id, render};

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
            confirmation_threshold: None,
            owners: vec!["a".to_owned(), "b".to_owned()],
            my_owner_key: None,
            participants: Vec::new(),
            non_hosting_owners: Vec::new(),
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
                tab_block(Tab::Parties, Some(("search: treasury▏".to_owned(), true))),
                "⠋",
            );
        });
        assert!(rendered.contains("search: treasury"));
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
                coordinator_instance: None,
                coordinator_name: None,
                expected_peers: Vec::new(),
                completed_peers: Vec::new(),
                connected_peers: Vec::new(),
                acs_progress: None,
                dec_party_id: None,
                prefix: None,
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
            }),
            FeedItem::Run(WorkflowRun {
                instance_name: "onboarding-done-abc".to_owned(),
                kind: WorkflowKind::Onboarding,
                role: WorkflowRole::Coordinator,
                status: WorkflowProgress::Completed,
                current_step: "Complete".to_owned(),
                step_index: 5,
                step_total: 6,
                config_json: String::new(),
                coordinator_pubkey: None,
                coordinator_instance: None,
                coordinator_name: None,
                expected_peers: Vec::new(),
                completed_peers: Vec::new(),
                connected_peers: Vec::new(),
                acs_progress: None,
                dec_party_id: None,
                prefix: None,
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
            }),
            FeedItem::Invitation(PendingInvitation {
                id: "inv-1".to_owned(),
                invitation_type: InvitationType::Onboarding,
                coordinator_pubkey: "1220deadbeef".to_owned(),
                coordinator_name: Some("alice".to_owned()),
                received_at: 0,
                prefix: Some("treasury-rc5".to_owned()),
                participants: Vec::new(),
                dar_filenames: Vec::new(),
                dar_hashes: Vec::new(),
                kicked_participant: None,
                kicked_owner_key: None,
                new_threshold: None,
                previous_threshold: None,
                confirmation_threshold: None,
                dec_party_id: None,
                new_participant: None,
                package_names: Vec::new(),
                contract_intents: Vec::new(),
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
        // Progress is 1-based: step index 2 of 3 renders as "3/3" (final step).
        assert!(rendered.contains("3/3"));
        assert!(rendered.contains("In progress"));
        // A finished run hides its step fraction (no "5/6" or "6/6"); the
        // status column carries the outcome instead.
        assert!(rendered.contains("Completed"));
        assert!(!rendered.contains("5/6"));
        assert!(!rendered.contains("6/6"));
        // The invitation row is present and actionable.
        assert!(rendered.contains("Onboarding"));
        assert!(rendered.contains("treasury-rc5"));
        assert!(rendered.contains("Invitation"));
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
