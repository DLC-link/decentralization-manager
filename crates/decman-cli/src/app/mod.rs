//! The terminal UI controller: the login loop, the background workers, the
//! App state, input handling and the actions behind each key.

mod composer_keys;
mod forms;
mod governance;
mod input;
mod login;
mod node;
mod overlay;
mod overlay_keys;
mod party;
mod state;
mod tabs;
mod worker;

pub use login::{Outcome, run_login};
pub use overlay::PeerForm;
pub use overlay::{ChangeThresholdForm, ComposerKind, ComposerPick, DeployForm};
pub use overlay::{DetailData, GovItem, GovView, GrantForm, IdpMode, NetworkEditState};
pub use overlay::{MemberChange, MemberChangeForm};
pub use overlay::{NodeInfoData, OnboardForm, Overlay, PartyConfigForm, PcRow, PeerChoice};
pub use state::App;
pub use tabs::{Status, Tab, TabView};
pub use worker::{PEER_POLL_INTERVAL, spawn_workers};

#[cfg(test)]
pub use overlay::KickCandidate;
