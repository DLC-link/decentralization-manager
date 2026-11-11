//! Node popups: party config, network edit, auth, grant and node info.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};

use crate::api::{AuthStatusKind, PartyAuthStatus};
use crate::app::PeerForm;
use crate::app::{GrantForm, IdpMode, NetworkEditState, NodeInfoData, PartyConfigForm, PcRow};
use crate::logo;

use crate::ui::popups::{detail_kv, detail_label};
use crate::ui::widgets::{centered_rect, dim_line, draw_shadow, popup_block, truncate};

/// A `label  value` form row; the focused row is highlighted with a cursor.
fn pc_line(label: &str, value: String, focused: bool) -> Line<'static> {
    let style = if focused {
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
        value
    };
    Line::from(vec![detail_label(label), Span::styled(shown, style)])
}

/// A masked secret row. Blank with a stored secret shows a keep hint.
fn pc_secret_line(label: &str, value: &str, has_secret: bool, focused: bool) -> Line<'static> {
    let style = if focused {
        Style::default()
            .fg(logo::ORANGE)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let masked = "•".repeat(value.chars().count());
    let shown = if focused {
        format!("{masked}▏")
    } else if value.is_empty() {
        if has_secret {
            "(set — blank keeps it)".to_owned()
        } else {
            "(none)".to_owned()
        }
    } else {
        masked
    };
    Line::from(vec![detail_label(label), Span::styled(shown, style)])
}

/// An action button row (Discover / Save).
fn pc_button(label: &str, focused: bool) -> Line<'static> {
    let style = if focused {
        Style::default()
            .fg(Color::Black)
            .bg(logo::ORANGE)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Line::from(Span::styled(format!(" {label} "), style))
}

/// The per-party IdP configuration editor popup (Keycloak / Auth0).
pub fn party_config_popup(frame: &mut Frame, area: Rect, form: &PartyConfigForm) {
    let rows = form.rows();
    let mut lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let focused = index == form.cursor;
            match row {
                PcRow::Mode => {
                    let provider = match form.mode {
                        IdpMode::Keycloak => "Keycloak",
                        IdpMode::Auth0 => "Auth0",
                    };
                    pc_line("Provider", format!("‹ {provider} ›"), focused)
                }
                PcRow::MemberParty => {
                    pc_line("Member party", form.member_party_id.clone(), focused)
                }
                PcRow::UserId => pc_line("User id", form.user_id.clone(), focused),
                PcRow::KcUrl => pc_line("Keycloak URL", form.kc_url.clone(), focused),
                PcRow::KcRealm => pc_line("Realm", form.kc_realm.clone(), focused),
                PcRow::KcClientId => pc_line("Client id", form.kc_client_id.clone(), focused),
                PcRow::KcSecret => pc_secret_line(
                    "Client secret",
                    &form.kc_secret,
                    form.kc_has_secret,
                    focused,
                ),
                PcRow::Auth0Domain => pc_line("Auth0 domain", form.auth0_domain.clone(), focused),
                PcRow::Auth0Audience => pc_line("Audience", form.auth0_audience.clone(), focused),
                PcRow::Auth0ClientId => pc_line("Client id", form.auth0_client_id.clone(), focused),
                PcRow::Auth0Secret => pc_secret_line(
                    "Client secret",
                    &form.auth0_secret,
                    form.auth0_has_secret,
                    focused,
                ),
                PcRow::Discover => pc_button("Discover member party", focused),
                PcRow::Submit => pc_button("Save", focused),
            }
        })
        .collect();
    if !form.status.is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled(
            form.status.clone(),
            Style::default().fg(Color::DarkGray),
        ));
    }

    let width = 76.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16).saturating_add(2)).clamp(6, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let title = format!("Party config · {}", form.party_name);
    let paragraph = Paragraph::new(lines).block(popup_block(&title));
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The network-config editor popup: the peer list, or the add-peer sub-form.
pub fn network_edit_popup(frame: &mut Frame, area: Rect, state: &NetworkEditState) {
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
    draw_shadow(frame, rect, area);
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
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The human-readable prefix of a Canton id (segment before `::`).
pub fn id_prefix(id: &str) -> &str {
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
pub fn auth_popup(frame: &mut Frame, area: Rect, parties: &[PartyAuthStatus], selected: usize) {
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
        // Only the selected party's header is highlighted, like the other lists.
        let header_style = if index == selected {
            Style::default()
                .fg(logo::ORANGE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        lines.push(Line::from(Span::styled(
            format!("{marker}{}", id_prefix(&party.dec_party_id)),
            header_style,
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
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The grant-rights form popup: admin client id + secret for one party.
pub fn grant_popup(frame: &mut Frame, area: Rect, form: &GrantForm) {
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
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// This node's Noise key status and DSO network identity. Both sections render
/// independently, so an unreachable DSO API still shows the local key status.
pub fn node_info_popup(frame: &mut Frame, area: Rect, data: &NodeInfoData) {
    let error_line =
        |error: &str| Line::styled(format!("Error: {error}"), Style::default().fg(Color::Red));

    let mut lines = vec![dim_line("Node keys")];
    match &data.key {
        Ok(status) => {
            lines.push(detail_kv(
                "Noise keys",
                if status.has_keys {
                    "present".to_owned()
                } else {
                    "not generated".to_owned()
                },
            ));
            if let Some(key) = status.public_key.as_deref().filter(|key| !key.is_empty()) {
                lines.push(detail_kv("Public key", key.to_owned()));
            }
        }
        Err(error) => lines.push(error_line(error)),
    }
    lines.push(Line::default());
    lines.push(dim_line("DSO network"));
    match &data.network {
        Ok(info) => {
            lines.push(detail_kv("DSO party", info.dso_party_id.clone()));
            if !info.amulet_rules_cid.is_empty() {
                lines.push(detail_kv(
                    "Amulet rules",
                    truncate(&info.amulet_rules_cid, 44),
                ));
            }
        }
        Err(error) => lines.push(error_line(error)),
    }

    let width = 80.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16) + 2).clamp(7, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(popup_block("Node info"));
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

#[cfg(test)]
mod tests {

    use crate::api::{AuthStatusKind, PartyAuthStatus};
    use crate::app::{GrantForm, IdpMode, NetworkEditState, NodeInfoData, Overlay};
    use crate::app::{PartyConfigForm, PeerForm};

    use crate::ui::fixtures::render;
    use crate::ui::frame::draw_overlay;

    #[test]
    fn node_info_popup_renders_keys_and_network() {
        use crate::api::{KeyStatus, NetworkInfo};

        let overlay = Overlay::NodeInfo(Box::new(NodeInfoData {
            key: Ok(KeyStatus {
                has_keys: true,
                public_key: Some("deadbeefcafe".to_owned()),
            }),
            network: Ok(NetworkInfo {
                dso_party_id: "DSO::1220".to_owned(),
                amulet_rules_cid: "00rules".to_owned(),
            }),
        }));
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Node info"));
        assert!(rendered.contains("present"));
        assert!(rendered.contains("deadbeefcafe"));
        assert!(rendered.contains("DSO"));
    }

    #[test]
    fn node_info_popup_shows_network_error_but_keeps_keys() {
        use crate::api::KeyStatus;

        let overlay = Overlay::NodeInfo(Box::new(NodeInfoData {
            key: Ok(KeyStatus {
                has_keys: false,
                public_key: None,
            }),
            network: Err("DSO API unreachable".to_owned()),
        }));
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("not generated"));
        assert!(rendered.contains("Error"));
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
    fn party_config_popup_renders_keycloak_fields_and_secret_hint() {
        let form = PartyConfigForm {
            dec_party_id: "cbtc-network::1220".to_owned(),
            party_name: "cbtc-network".to_owned(),
            mode: IdpMode::Keycloak,
            member_party_id: "member::1220".to_owned(),
            user_id: "user-1".to_owned(),
            kc_url: "https://kc".to_owned(),
            kc_realm: "realm".to_owned(),
            kc_client_id: "client".to_owned(),
            kc_secret: String::new(),
            kc_has_secret: true,
            auth0_domain: String::new(),
            auth0_audience: String::new(),
            auth0_client_id: String::new(),
            auth0_secret: String::new(),
            auth0_has_secret: false,
            // Focus the member-party row so the secret line renders unfocused.
            cursor: 1,
            status: String::new(),
        };
        let overlay = Overlay::PartyConfig(Box::new(form));
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Party config"));
        assert!(rendered.contains("Keycloak"));
        assert!(rendered.contains("Member party"));
        assert!(rendered.contains("Client id"));
        // A stored secret with a blank field shows the keep hint, never a value.
        assert!(rendered.contains("blank keeps it"));
        assert!(rendered.contains("Save"));
    }
}
