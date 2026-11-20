//! Tabs, status, filters, and the search and selection helpers.

use ratatui::layout::Rect;
use ratatui::widgets::TableState;

use common::types::{DecentralizedParty, VettedPackageInfo};

use crate::api::{FeedItem, PeerView, party_name};

use crate::app::state::App;
use crate::app::worker::Request;

/// The selectable top-level views.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Parties,
    Peers,
    Dars,
    Workflows,
}

impl Tab {
    /// All tabs, in display order.
    pub const ALL: [Tab; 4] = [Tab::Parties, Tab::Peers, Tab::Dars, Tab::Workflows];

    /// Title shown in the tab bar.
    pub fn title(self) -> &'static str {
        match self {
            Tab::Parties => "Parties",
            Tab::Peers => "Peers",
            Tab::Dars => "Dars",
            Tab::Workflows => "Workflows",
        }
    }

    /// The next tab, wrapping around.
    pub(super) fn next(self) -> Tab {
        let all = Tab::ALL;
        let index = all.iter().position(|t| *t == self).unwrap_or(0);
        all[(index + 1) % all.len()]
    }

    /// The previous tab, wrapping around.
    pub(super) fn previous(self) -> Tab {
        let all = Tab::ALL;
        let index = all.iter().position(|t| *t == self).unwrap_or(0);
        all[(index + all.len() - 1) % all.len()]
    }

    /// Whether this tab's table can be searched.
    pub(super) fn searchable(self) -> bool {
        matches!(self, Tab::Parties | Tab::Dars)
    }

    /// The on-demand fetch request for this tab, if any (peers auto-refresh).
    fn request(self) -> Option<Request> {
        match self {
            Tab::Parties => Some(Request::Parties),
            Tab::Dars => Some(Request::Dars),
            Tab::Workflows => Some(Request::Feed),
            Tab::Peers => None,
        }
    }
}

/// State of a tab's data fetch, driving what the UI renders.
#[derive(Debug)]
pub enum Status {
    Loading,
    Loaded,
    Error(String),
}

/// Borrowed render data for the active tab, handed to the UI layer. Parties and
/// Dars are pre-filtered by their search query.
pub enum TabView<'a> {
    Parties(&'a Status, Vec<&'a DecentralizedParty>, &'a mut TableState),
    Peers(&'a Status, &'a [PeerView], &'a mut TableState),
    Dars(&'a Status, Vec<&'a VettedPackageInfo>, &'a mut TableState),
    Workflows(&'a Status, &'a [FeedItem], &'a mut TableState),
}

/// Case-insensitive substring match of `query` against any of `fields`. An
/// empty query matches everything.
fn query_matches(query: &str, fields: &[&str]) -> bool {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    fields
        .iter()
        .any(|field| field.to_lowercase().contains(&needle))
}

/// Parties matching `query` (by name or party id).
pub(super) fn filter_parties<'a>(
    parties: &'a [DecentralizedParty],
    query: &str,
) -> Vec<&'a DecentralizedParty> {
    parties
        .iter()
        .filter(|party| {
            let party_id = party.party_id.to_string();
            query_matches(query, &[party_name(party), &party_id])
        })
        .collect()
}

/// Packages matching `query` (by name or package id).
pub(super) fn filter_dars<'a>(
    dars: &'a [VettedPackageInfo],
    query: &str,
) -> Vec<&'a VettedPackageInfo> {
    dars.iter()
        .filter(|package| query_matches(query, &[&package.package_name, &package.package_id]))
        .collect()
}

/// The tab whose label on the panel's top border contains screen column
/// `column`, mirroring the tab-bar layout in [`ui::tab_block`]: a leading space,
/// then each tab title separated by a 3-column ` │ `.
pub(super) fn tab_at(body: Rect, column: u16) -> Option<Tab> {
    let mut offset: u16 = 1; // the leading space before the first title
    for tab in Tab::ALL {
        let title_len = tab.title().chars().count() as u16;
        let start = body.x + 1 + offset; // +1 for the top-left border corner
        if column >= start && column < start + title_len {
            return Some(tab);
        }
        offset += title_len + 3; // advance past the title and the ` │ ` divider
    }
    None
}

/// Keep a table's selection in range after its data changes.
pub(super) fn clamp_selection(table: &mut TableState, len: usize) {
    let selected = match table.selected() {
        Some(index) if index < len => Some(index),
        _ => (len > 0).then_some(0),
    };
    table.select(selected);
}

impl App {
    /// Request a fresh fetch of the active tab (peers refresh on their own).
    pub(super) fn refresh_active(&mut self) {
        if let Some(request) = self.active_tab.request() {
            let _ = self.requests.send(request);
        }
    }

    /// Switch tabs. Data is prefetched and cached, so this is instant.
    pub(super) fn switch_to(&mut self, tab: Tab) {
        if self.active_tab == tab {
            return;
        }
        self.searching = false;
        self.active_tab = tab;
    }

    /// The selected item on the Workflows feed.
    pub(super) fn selected_feed_item(&self) -> Option<&FeedItem> {
        let index = self.feed_table.selected()?;
        self.feed.get(index)
    }

    pub(super) fn current_query(&self) -> &str {
        match self.active_tab {
            Tab::Parties => &self.parties_query,
            Tab::Dars => &self.dars_query,
            _ => "",
        }
    }

    fn current_query_mut(&mut self) -> Option<&mut String> {
        match self.active_tab {
            Tab::Parties => Some(&mut self.parties_query),
            Tab::Dars => Some(&mut self.dars_query),
            _ => None,
        }
    }

    pub(super) fn query_push(&mut self, c: char) {
        if let Some(query) = self.current_query_mut() {
            query.push(c);
        }
    }

    pub(super) fn query_pop(&mut self) {
        if let Some(query) = self.current_query_mut() {
            query.pop();
        }
    }

    pub(super) fn set_query(&mut self, value: String) {
        if let Some(query) = self.current_query_mut() {
            *query = value;
        }
    }

    pub(super) fn current_len(&self) -> usize {
        match self.active_tab {
            Tab::Parties => filter_parties(&self.parties, &self.parties_query).len(),
            Tab::Peers => self.peers.len(),
            Tab::Dars => filter_dars(&self.dars, &self.dars_query).len(),
            Tab::Workflows => self.feed.len(),
        }
    }

    pub(super) fn current_table_mut(&mut self) -> &mut TableState {
        match self.active_tab {
            Tab::Parties => &mut self.parties_table,
            Tab::Peers => &mut self.peers_table,
            Tab::Dars => &mut self.dars_table,
            Tab::Workflows => &mut self.feed_table,
        }
    }

    /// Reset the active tab's selection to the first row (or none when empty).
    pub(super) fn reset_selection(&mut self) {
        let len = self.current_len();
        self.current_table_mut().select((len > 0).then_some(0));
    }

    /// Move the selection to the next row, wrapping at the end.
    pub(super) fn select_next(&mut self) {
        let len = self.current_len();
        if len == 0 {
            return;
        }
        let table = self.current_table_mut();
        let next = table.selected().map_or(0, |i| (i + 1) % len);
        table.select(Some(next));
    }

    /// Move the selection to the previous row, wrapping at the start.
    pub(super) fn select_previous(&mut self) {
        let len = self.current_len();
        if len == 0 {
            return;
        }
        let table = self.current_table_mut();
        let previous = table.selected().map_or(0, |i| (i + len - 1) % len);
        table.select(Some(previous));
    }
}

#[cfg(test)]
mod tests {
    use common::canton_id::CantonId;

    use ratatui::layout::Rect;
    use ratatui::widgets::TableState;

    use common::types::DecentralizedParty;

    use crate::api::party_name;

    use super::*;

    /// A valid 34-byte (68 hex char) namespace for building realistic Canton ids.
    const NS: &str = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";

    fn party(name: &str) -> DecentralizedParty {
        DecentralizedParty {
            party_id: CantonId::parse(&format!("{name}::{NS}")).unwrap(),
            threshold: 1,
            confirmation_threshold: None,
            owners: Vec::new(),
            my_owner_key: None,
            participants: Vec::new(),
            non_hosting_owners: Vec::new(),
            contracts: Vec::new(),
            local_metadata: None,
        }
    }

    #[test]
    fn filter_parties_matches_name_case_insensitively() {
        let parties = [
            party("cbtc-network"),
            party("treasury-rc5"),
            party("test-net"),
        ];

        let all = filter_parties(&parties, "");
        let treasury = filter_parties(&parties, "TREASURY");

        assert_eq!(all.len(), 3);
        assert_eq!(treasury.len(), 1);
        assert_eq!(party_name(treasury[0]), "treasury-rc5");
    }

    #[test]
    fn filter_parties_matches_party_id() {
        let parties = [party("alpha")];
        // A substring of the namespace hex matches against the full party id.
        assert_eq!(filter_parties(&parties, "c4010d").len(), 1);
        assert_eq!(filter_parties(&parties, "nomatch").len(), 0);
    }

    #[test]
    fn tab_at_maps_border_columns_to_tabs() {
        // Title bar: "‹space›Parties │ Peers │ Dars │ Workflows", starting one
        // column in from the top-left border corner.
        let body = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 20,
        };
        // The corner and the leading space are not a tab.
        assert_eq!(tab_at(body, 0), None);
        assert_eq!(tab_at(body, 1), None);
        // "Parties" spans columns 2..9.
        assert_eq!(tab_at(body, 2), Some(Tab::Parties));
        assert_eq!(tab_at(body, 8), Some(Tab::Parties));
        // The " │ " divider is dead space.
        assert_eq!(tab_at(body, 9), None);
        // The remaining tabs, past each divider.
        assert_eq!(tab_at(body, 12), Some(Tab::Peers));
        assert_eq!(tab_at(body, 20), Some(Tab::Dars));
        assert_eq!(tab_at(body, 27), Some(Tab::Workflows));
        assert_eq!(tab_at(body, 35), Some(Tab::Workflows));
        // Past the last title.
        assert_eq!(tab_at(body, 60), None);
    }

    #[test]
    fn tab_at_respects_panel_offset() {
        // A non-zero panel origin shifts every tab range by the same amount.
        let body = Rect {
            x: 10,
            y: 4,
            width: 80,
            height: 20,
        };
        assert_eq!(tab_at(body, 12), Some(Tab::Parties));
        assert_eq!(tab_at(body, 22), Some(Tab::Peers));
    }

    #[test]
    fn clamp_selection_preserves_or_resets() {
        let mut table = TableState::default();

        clamp_selection(&mut table, 0);
        assert_eq!(table.selected(), None);

        clamp_selection(&mut table, 3);
        assert_eq!(table.selected(), Some(0));

        table.select(Some(2));
        clamp_selection(&mut table, 3);
        assert_eq!(table.selected(), Some(2));

        clamp_selection(&mut table, 1);
        assert_eq!(table.selected(), Some(0));
    }
}
