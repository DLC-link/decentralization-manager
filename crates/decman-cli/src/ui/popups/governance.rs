//! Governance popups: approvals, the composer and the deploy form.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{ComposerKind, ComposerPick, DeployForm, GovItem, GovView};
use crate::composer::{Composer, FieldKind};
use crate::logo;

use crate::ui::detail::format_micros_human;
use crate::ui::popups::{detail_item, detail_kv, detail_label, id_prefix};
use crate::ui::widgets::{centered_rect, dim_line, draw_shadow, popup_block, truncate};

/// The governance-core contract deployment popup: resolved context plus the two
/// editable fields (threshold, timeout) and a submit row.
pub fn deploy_popup(frame: &mut Frame, area: Rect, form: &DeployForm) {
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
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

/// The action / proposal type picker popup.
pub fn composer_pick_popup(frame: &mut Frame, area: Rect, pick: &ComposerPick) {
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
    draw_shadow(frame, rect, area);
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
        FieldKind::Picker {
            options, loaded, ..
        } => {
            if let Some(option) = options.iter().find(|option| option.value == field.value) {
                // A selected option: show its human label, not the raw cid.
                format!("‹ {} ›", option.label)
            } else if !field.value.is_empty() {
                // A pasted / typed cid with no matching option.
                if focused {
                    format!("{}▏", field.value)
                } else {
                    field.value.clone()
                }
            } else if !loaded {
                "loading…".to_owned()
            } else if options.is_empty() {
                // Nothing on the ledger to pick — fall back to text entry.
                if focused {
                    "▏".to_owned()
                } else {
                    "(none — type a cid)".to_owned()
                }
            } else if focused {
                format!("‹ pick 1 of {} ›", options.len())
            } else {
                "(none selected)".to_owned()
            }
        }
    }
}

/// The composer form popup: one row per field plus a virtual submit row.
pub fn composer_popup(frame: &mut Frame, area: Rect, composer: &Composer) {
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
    draw_shadow(frame, rect, area);
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
        _ => None,
    };
    match detail {
        Some(detail) => format!("{base}  {detail}"),
        None => base,
    }
}

/// The governance approvals popup: a selectable list of pending actions.
pub fn governance_popup(frame: &mut Frame, area: Rect, view: &GovView) {
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
    draw_shadow(frame, rect, area);
    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

#[cfg(test)]
mod tests {

    use crate::app::{DeployForm, GovItem, GovView, Overlay};
    use crate::composer::{Composer, FieldKind};

    use super::*;
    use crate::ui::fixtures::render;
    use crate::ui::frame::draw_overlay;

    #[test]
    fn composer_field_display_picker_states() {
        use crate::composer::{ComposerField, PickerOption, PickerSource};

        let make = |kind, value: &str| ComposerField {
            key: "mint_request_cid",
            label: "Mint request",
            kind,
            value: value.to_owned(),
            optional: false,
            help: "",
        };

        // Still loading.
        let loading = make(
            FieldKind::Picker {
                source: PickerSource::MintRequests,
                options: Vec::new(),
                loaded: false,
            },
            "",
        );
        assert_eq!(composer_field_display(&loading, false), "loading…");

        // Loaded but empty → text-entry fallback.
        let empty = make(
            FieldKind::Picker {
                source: PickerSource::MintRequests,
                options: Vec::new(),
                loaded: true,
            },
            "",
        );
        assert_eq!(composer_field_display(&empty, false), "(none — type a cid)");

        // A selected option shows its human label, not the raw cid.
        let selected = make(
            FieldKind::Picker {
                source: PickerSource::MintRequests,
                options: vec![PickerOption {
                    value: "00abc".to_owned(),
                    label: "100 CBTC → alice".to_owned(),
                }],
                loaded: true,
            },
            "00abc",
        );
        assert_eq!(
            composer_field_display(&selected, false),
            "‹ 100 CBTC → alice ›"
        );
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
                &serde_json::json!({ "type": "governance_add_member", "member": "alice::1220" })
            ),
            "Governance Add Member  alice"
        );
        // An action with no detail arm renders the humanized type alone.
        assert_eq!(
            gov_action_summary(&serde_json::json!({ "type": "utility_setup" })),
            "Utility Setup"
        );
    }

    #[test]
    fn governance_popup_renders_offchain_and_domain() {
        use crate::api::{DomainGovAction, GovAction};

        let view = GovView {
            party_name: "cbtc-network".to_owned(),
            party_id: "dec::1220".to_owned(),
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
                    executable_confirmation_cids: None,
                    confirmation_count: 1,
                    can_execute: false,
                }),
                GovItem::Domain(DomainGovAction {
                    proposal_cid: "00prop".to_owned(),
                    action_label: "SetupCcPreapproval".to_owned(),
                    description: Some("set up preapproval".to_owned()),
                    confirmations: Vec::new(),
                    executable_confirmation_cids: None,
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
            rules_contract_id: "00r".to_owned(),
            default_threshold: 2,
            options: crate::composer::action_types(),
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
}
