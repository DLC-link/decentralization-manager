//! The demo wallet: a local web app that drives [`crate`] end to end.
//!
//! It exists to show co-validation working — name a party, watch it come up on
//! every host, transact as it — and to be the reference for a wallet provider
//! integrating the tenant API.
//!
//! Why the key lives in this process rather than in the browser: a wallet holds
//! two secrets, the party's signing key and the provider's tenant API key.
//! Neither belongs in a page. So the browser here is glass — it renders state and
//! posts intents — and this process is the wallet. That also keeps one
//! implementation of the crypto (the library) instead of a second one in
//! TypeScript, and sidesteps CORS: DecMan is same-origin-only by default, so a
//! page cannot call three hosts directly anyway.

use std::{path::PathBuf, sync::Mutex};

use common::canton_id::CantonId;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{ExternalKeyPair, Result, TenantClient, WalletHost};

pub mod api;
mod assets;
mod server;

pub use server::run;

/// A host as the UI sees it.
#[derive(Clone, Debug, Serialize)]
pub struct HostView {
    pub base_url: String,
    pub participant_id: String,
}

/// One host as configured on the command line.
#[derive(Clone, Debug)]
pub struct HostConfig {
    pub base_url: String,
    pub participant_id: CantonId,
}

/// Everything the demo needs to talk to a hosting set.
pub struct DemoConfig {
    pub hosts: Vec<HostConfig>,
    pub api_key: String,
    /// Confirmation threshold to request. `None` lets DecMan default it to `N-1`.
    pub confirmation_threshold: Option<u32>,
    /// Where to persist the party's key between runs. `None` keeps it in memory
    /// only, so restarting starts a fresh demo.
    pub state_file: Option<PathBuf>,
}

/// The party this wallet currently holds. The seed is the only secret, and it is
/// wiped on drop.
struct WalletState {
    seed: Zeroizing<[u8; 32]>,
    party_hint: String,
    party_id: String,
}

/// What gets written to the state file. Deliberately explicit that it is secret.
#[derive(Deserialize, Serialize)]
struct PersistedWallet {
    /// Base64-encoded 32-byte Ed25519 seed. This is the party's private key.
    seed: String,
    party_hint: String,
}

/// Shared demo state: the configured hosting set plus the one party in play.
pub struct DemoState {
    hosts: Vec<WalletHost>,
    host_views: Vec<HostView>,
    confirmation_threshold: Option<u32>,
    state_file: Option<PathBuf>,
    wallet: Mutex<Option<WalletState>>,
}

impl DemoState {
    /// Build the state, restoring a previously persisted party if there is one.
    ///
    /// # Errors
    /// Fails if a host's HTTP client cannot be built.
    pub fn new(config: DemoConfig) -> Result<Self> {
        let mut hosts = Vec::with_capacity(config.hosts.len());
        let mut host_views = Vec::with_capacity(config.hosts.len());
        for host in &config.hosts {
            let client = TenantClient::new(host.base_url.clone(), config.api_key.clone())?;
            host_views.push(HostView {
                base_url: client.base_url().to_string(),
                participant_id: host.participant_id.to_string(),
            });
            hosts.push(WalletHost::new(client, host.participant_id.clone()));
        }

        let restored = config
            .state_file
            .as_deref()
            .and_then(|path| match load_state(path) {
                Ok(state) => state,
                Err(e) => {
                    tracing::warn!("ignoring unreadable state file {path:?}: {e}");
                    None
                }
            });
        if let Some(state) = &restored {
            tracing::info!(party_id = %state.party_id, "restored party from state file");
        }

        Ok(Self {
            hosts,
            host_views,
            confirmation_threshold: config.confirmation_threshold,
            state_file: config.state_file,
            wallet: Mutex::new(restored),
        })
    }

    pub fn hosts(&self) -> &[WalletHost] {
        &self.hosts
    }

    pub fn host_views(&self) -> &[HostView] {
        &self.host_views
    }

    pub fn confirmation_threshold(&self) -> Option<u32> {
        self.confirmation_threshold
    }

    /// The party's private seed, base64-encoded. Only the wallet's own UI sees this,
    /// over loopback — it is never sent to a DecMan host. Exposed because the whole
    /// point of the demo is showing that the key is the owner's.
    fn seed_b64(&self) -> Option<zeroize::Zeroizing<String>> {
        let guard = self.lock();
        let state = guard.as_ref()?;
        Some(ExternalKeyPair::from_seed(*state.seed).seed_b64())
    }

    /// The party's key, hint, and id — rebuilt from the stored seed so callers can
    /// do async work without holding the lock.
    fn current(&self) -> Option<(ExternalKeyPair, String, String)> {
        let guard = self.lock();
        let state = guard.as_ref()?;
        Some((
            ExternalKeyPair::from_seed(*state.seed),
            state.party_hint.clone(),
            state.party_id.clone(),
        ))
    }

    /// Record a freshly onboarded party, persisting it if a state file is set.
    fn store(&self, key: &ExternalKeyPair, party_hint: &str, party_id: &str) {
        let state = WalletState {
            seed: key.seed(),
            party_hint: party_hint.to_string(),
            party_id: party_id.to_string(),
        };
        if let Some(path) = self.state_file.as_deref()
            && let Err(e) = save_state(path, key, party_hint)
        {
            tracing::warn!("could not persist the wallet to {path:?}: {e}");
        }
        *self.lock() = Some(state);
    }

    /// Forget the party so the next demo starts clean.
    fn clear(&self) {
        *self.lock() = None;
        if let Some(path) = self.state_file.as_deref()
            && path.exists()
            && let Err(e) = std::fs::remove_file(path)
        {
            tracing::warn!("could not remove the state file {path:?}: {e}");
        }
    }

    /// A poisoned mutex still holds readable demo state, and panicking a second
    /// time would take the whole demo down mid-presentation.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<WalletState>> {
        match self.wallet.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

fn load_state(path: &std::path::Path) -> std::io::Result<Option<WalletState>> {
    if !path.exists() {
        return Ok(None);
    }
    let contents = Zeroizing::new(std::fs::read_to_string(path)?);
    let persisted: PersistedWallet = serde_json::from_str(&contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let key = ExternalKeyPair::from_seed_b64(&persisted.seed)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(WalletState {
        seed: key.seed(),
        party_id: key.party_id(&persisted.party_hint),
        party_hint: persisted.party_hint,
    }))
}

/// Write the seed with owner-only permissions — it is the party's private key.
fn save_state(
    path: &std::path::Path,
    key: &ExternalKeyPair,
    party_hint: &str,
) -> std::io::Result<()> {
    use std::io::Write;

    let body = Zeroizing::new(
        serde_json::to_string_pretty(&PersistedWallet {
            seed: key.seed_b64().to_string(),
            party_hint: party_hint.to_string(),
        })
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
    );

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(body.as_bytes())
}
