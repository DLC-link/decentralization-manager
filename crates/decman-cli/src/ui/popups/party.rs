//! Party workflow popups: onboard, kick, add party and change threshold.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{ChangeThresholdForm, MemberChange, MemberChangeForm, OnboardForm};
use crate::logo;

use crate::ui::widgets::{centered_rect, dim_line, draw_shadow, popup_block, threshold_line};

/// The onboarding (create-party) form popup: a prefix field above a tickable
/// peer list, with the focused element highlighted.
pub fn onboard_popup(frame: &mut Frame, area: Rect, form: &OnboardForm) {
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
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The kick-participant and add-party form popup: pick a participant to
/// remove or add, and set the new signing threshold.
pub fn member_change_popup(frame: &mut Frame, area: Rect, form: &MemberChangeForm) {
    let (title, heading) = match form.kind {
        MemberChange::Kick => ("Kick participant", "Remove participant:"),
        MemberChange::Add => ("Add party member", "Add participant:"),
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Party  ", Style::default().fg(Color::DarkGray)),
            Span::raw(form.party_name.clone()),
        ]),
        Line::default(),
        dim_line(heading),
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
    lines.push(threshold_line(
        form.new_threshold,
        form.previous_threshold,
        form.max_threshold,
    ));

    let width = 64.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16) + 2).clamp(8, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines).block(popup_block(title));
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The change-threshold form popup: adjust a party's signing threshold.
pub fn change_threshold_popup(frame: &mut Frame, area: Rect, form: &ChangeThresholdForm) {
    let lines = vec![
        Line::from(vec![
            Span::styled("Party  ", Style::default().fg(Color::DarkGray)),
            Span::raw(form.party_name.clone()),
        ]),
        Line::default(),
        dim_line("Membership is unchanged; only the signing threshold changes."),
        Line::default(),
        threshold_line(
            form.new_threshold,
            form.previous_threshold,
            form.max_threshold,
        ),
    ];

    let width = 64.min(area.width.saturating_sub(4));
    let height = ((lines.len() as u16) + 2).clamp(7, area.height.saturating_sub(4));
    let rect = centered_rect(width, height, area);
    let paragraph = Paragraph::new(lines).block(popup_block("Change threshold"));
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

#[cfg(test)]
mod tests {

    use crate::app::PeerChoice;
    use crate::app::{ChangeThresholdForm, MemberChange, MemberChangeForm, OnboardForm, Overlay};

    use crate::ui::fixtures::render;
    use crate::ui::frame::draw_overlay;

    #[test]
    fn onboard_popup_renders_prefix_and_peers() {
        let overlay = Overlay::Onboard(OnboardForm {
            prefix: "treasury".to_owned(),
            peers: vec![PeerChoice {
                id: "p1::1220".to_owned(),
                name: "alpha".to_owned(),
                checked: true,
            }],
            cursor: 0,
        });
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Onboard new party"));
        assert!(rendered.contains("treasury"));
        assert!(rendered.contains("alpha"));
        assert!(rendered.contains("[x]"));
    }

    #[test]
    fn kick_popup_renders_candidates_and_threshold() {
        use crate::app::KickCandidate;

        let overlay = Overlay::MemberChange(MemberChangeForm {
            kind: MemberChange::Kick,
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
    fn add_party_popup_renders_candidates_and_threshold() {
        use crate::app::KickCandidate;

        let overlay = Overlay::MemberChange(MemberChangeForm {
            kind: MemberChange::Add,
            party_id: "dec::1220".to_owned(),
            party_name: "cbtc-network".to_owned(),
            previous_threshold: 2,
            candidates: vec![KickCandidate {
                participant_id: "p::1220".to_owned(),
                label: "peerNew".to_owned(),
            }],
            selected: 0,
            new_threshold: 3,
            max_threshold: 3,
        });
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Add party member"));
        assert!(rendered.contains("cbtc-network"));
        assert!(rendered.contains("peerNew"));
        assert!(rendered.contains("New threshold"));
    }

    #[test]
    fn change_threshold_popup_renders_threshold_bounds() {
        let overlay = Overlay::ChangeThreshold(ChangeThresholdForm {
            party_id: "dec::1220".to_owned(),
            party_name: "cbtc-network".to_owned(),
            previous_threshold: 2,
            new_threshold: 3,
            max_threshold: 4,
        });
        let rendered = render(|frame, area| draw_overlay(frame, area, &overlay, "⠋"));
        assert!(rendered.contains("Change threshold"));
        assert!(rendered.contains("cbtc-network"));
        assert!(rendered.contains("(was 2, max 4)"));
    }
}
