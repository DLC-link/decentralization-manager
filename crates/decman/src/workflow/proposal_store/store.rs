use std::{
    future::Future,
    time::{SystemTime, UNIX_EPOCH},
};

use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
    CreateTemporaryTopologyStoreRequest, DropTemporaryTopologyStoreRequest,
    ListAvailableStoresRequest, StoreId, store_id,
    topology_manager_read_service_client::TopologyManagerReadServiceClient,
    topology_manager_write_service_client::TopologyManagerWriteServiceClient,
};
use tokio::runtime::Handle;
use tonic::transport::Channel;

use crate::{config::NodeConfig, error::Result};

/// Every temporary store this tool creates is named with this prefix, so a
/// sweep never touches a store something else created.
pub const STORE_PREFIX: &str = "decman-proposals-";

/// Canton caps a temporary store name at 185 characters. The instance part is
/// cut well short of that, leaving room for the prefix and the attempt suffix.
const MAX_INSTANCE_CHARS: usize = 120;

/// Drop every temporary store an earlier process of this tool left behind.
///
/// A store outlives its run only when the process dies between creating and
/// dropping it. Canton keeps temporary stores in memory, so a participant
/// restart clears them too, but a participant can run for months. Call this
/// once at boot, before any run resumes: a sweep that ran alongside a resumed
/// run would drop that run's store.
///
/// Assumes one instance of this tool per participant, which is how it is
/// deployed.
///
/// # Errors
///
/// Errors when the participant's stores cannot be listed.
pub async fn drop_leftover_stores(config: &NodeConfig) -> Result<usize> {
    let stores = CantonStores {
        channel: config.admin_channel().await?,
    };
    drop_where(&stores, |name| name.starts_with(STORE_PREFIX)).await
}

/// The admin calls a temporary store's lifecycle needs. The Canton admin API
/// is the only production implementation; tests substitute a recorder.
pub(super) trait StoreAdmin: Clone + Send + Sync + 'static {
    fn temporary_store_names(&self) -> impl Future<Output = Result<Vec<String>>> + Send;
    fn create(&self, name: &str, protocol_version: u32) -> impl Future<Output = Result> + Send;
    fn drop_store(&self, name: &str) -> impl Future<Output = Result> + Send;
}

#[derive(Clone)]
pub(super) struct CantonStores {
    pub(super) channel: Channel,
}

impl StoreAdmin for CantonStores {
    async fn temporary_store_names(&self) -> Result<Vec<String>> {
        let response = TopologyManagerReadServiceClient::new(self.channel.clone())
            .list_available_stores(tonic::Request::new(ListAvailableStoresRequest {}))
            .await?
            .into_inner();
        Ok(response
            .store_ids
            .into_iter()
            .filter_map(|id| match id.store {
                Some(store_id::Store::Temporary(temporary)) => Some(temporary.name),
                _ => None,
            })
            .collect())
    }

    async fn create(&self, name: &str, protocol_version: u32) -> Result {
        TopologyManagerWriteServiceClient::new(self.channel.clone())
            .create_temporary_topology_store(tonic::Request::new(
                CreateTemporaryTopologyStoreRequest {
                    name: name.to_string(),
                    protocol_version,
                },
            ))
            .await?;
        Ok(())
    }

    async fn drop_store(&self, name: &str) -> Result {
        TopologyManagerWriteServiceClient::new(self.channel.clone())
            .drop_temporary_topology_store(tonic::Request::new(DropTemporaryTopologyStoreRequest {
                store_id: Some(store_id::Temporary {
                    name: name.to_string(),
                }),
            }))
            .await?;
        Ok(())
    }
}

/// A temporary store that is dropped however its owner exits.
///
/// [`ProposalStore::close`] drops it on the paths that return. A task aborted
/// mid-run (a cancelled workflow) never reaches `close`, so `Drop` hands the
/// drop to the runtime instead.
struct ProposalStore<A: StoreAdmin> {
    admin: A,
    name: String,
    open: bool,
}

impl<A: StoreAdmin> ProposalStore<A> {
    /// Drop what an earlier attempt of the same run left, then create a fresh
    /// store for this attempt.
    async fn open(admin: A, instance_name: &str, protocol_version: u32) -> Result<Self> {
        let earlier = instance_prefix(instance_name);
        if let Err(e) = drop_where(&admin, |name| name.starts_with(&earlier)).await {
            tracing::warn!(
                instance_name,
                error = format!("{e:#}"),
                "Could not look for temporary topology stores an earlier attempt of this run \
                 left behind"
            );
        }
        let name = store_name(instance_name, attempt_suffix());
        admin.create(&name, protocol_version).await?;
        tracing::debug!(store = %name, "Created temporary topology store");
        Ok(Self {
            admin,
            name,
            open: true,
        })
    }

    async fn close(mut self) {
        let result = self.admin.drop_store(&self.name).await;
        // Only now: a task aborted while the drop is in flight still leaves
        // the guard armed.
        self.open = false;
        match result {
            Ok(()) => tracing::debug!(store = %self.name, "Dropped temporary topology store"),
            Err(e) => tracing::warn!(
                store = %self.name,
                error = format!("{e:#}"),
                "Could not drop temporary topology store; the next attempt of this run, or the \
                 next boot, drops it"
            ),
        }
    }
}

impl<A: StoreAdmin> Drop for ProposalStore<A> {
    fn drop(&mut self) {
        if !self.open {
            return;
        }
        let admin = self.admin.clone();
        let name = std::mem::take(&mut self.name);
        match Handle::try_current() {
            Ok(runtime) => {
                runtime.spawn(async move {
                    match admin.drop_store(&name).await {
                        Ok(()) => tracing::info!(
                            store = %name,
                            "Dropped temporary topology store after its run stopped"
                        ),
                        Err(e) => tracing::warn!(
                            store = %name,
                            error = format!("{e:#}"),
                            "Could not drop temporary topology store after its run stopped; \
                             the next boot drops it"
                        ),
                    }
                });
            }
            Err(_) => tracing::warn!(
                store = %name,
                "Temporary topology store outlived its runtime; the next boot drops it"
            ),
        }
    }
}

/// Run `body` against a fresh temporary store for `instance_name`, and drop
/// the store whatever `body` returns.
pub(super) async fn in_store<A, T, F, Fut>(
    admin: A,
    instance_name: &str,
    protocol_version: u32,
    body: F,
) -> Result<T>
where
    A: StoreAdmin,
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let store = ProposalStore::open(admin, instance_name, protocol_version).await?;
    let result = body(store.name.clone()).await;
    store.close().await;
    result
}

/// Drop every temporary store whose name `matches`, and return how many went.
/// A store that will not drop is logged and skipped, so one bad store does not
/// keep the rest.
async fn drop_where<A: StoreAdmin>(admin: &A, matches: impl Fn(&str) -> bool) -> Result<usize> {
    let mut dropped = 0;
    for name in admin.temporary_store_names().await? {
        if !matches(&name) {
            continue;
        }
        match admin.drop_store(&name).await {
            Ok(()) => {
                tracing::info!(store = %name, "Dropped leftover temporary topology store");
                dropped += 1;
            }
            Err(e) => {
                tracing::warn!(
                    store = %name,
                    error = format!("{e:#}"),
                    "Could not drop leftover temporary topology store"
                )
            }
        }
    }
    Ok(dropped)
}

/// The name every store of `instance_name` starts with.
fn instance_prefix(instance_name: &str) -> String {
    let instance: String = instance_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(MAX_INSTANCE_CHARS)
        .collect();
    format!("{STORE_PREFIX}{instance}~")
}

/// A store name for one attempt of `instance_name`.
///
/// Each attempt gets its own name. A drop the guard spawned for an aborted
/// attempt can then never land on the store of the attempt after it.
fn store_name(instance_name: &str, attempt: u128) -> String {
    format!("{prefix}{attempt}", prefix = instance_prefix(instance_name))
}

fn attempt_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default()
}

pub(super) fn temporary_store_id(name: &str) -> StoreId {
    StoreId {
        store: Some(store_id::Store::Temporary(store_id::Temporary {
            name: name.to_string(),
        })),
    }
}

pub(super) fn next_serial(what: &str, head: u32) -> Result<u32> {
    head.checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("The {what} serial {head} has no successor"))
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use tokio::sync::Notify;

    use super::*;

    /// A participant's temporary stores, kept in memory, with every call
    /// recorded in order.
    #[derive(Clone, Default)]
    struct Recorder {
        stores: Arc<Mutex<BTreeSet<String>>>,
        calls: Arc<Mutex<Vec<String>>>,
        refuse_create: bool,
    }

    impl Recorder {
        fn with_stores(names: &[&str]) -> Self {
            let recorder = Self::default();
            if let Ok(mut stores) = recorder.stores.lock() {
                stores.extend(names.iter().map(|name| (*name).to_string()));
            }
            recorder
        }

        fn stores(&self) -> Result<Vec<String>> {
            Ok(self
                .stores
                .lock()
                .map_err(|_| anyhow::anyhow!("poisoned"))?
                .iter()
                .cloned()
                .collect())
        }

        fn calls(&self) -> Result<Vec<String>> {
            Ok(self
                .calls
                .lock()
                .map_err(|_| anyhow::anyhow!("poisoned"))?
                .clone())
        }

        fn record(&self, call: String) -> Result {
            self.calls
                .lock()
                .map_err(|_| anyhow::anyhow!("poisoned"))?
                .push(call);
            Ok(())
        }
    }

    impl StoreAdmin for Recorder {
        async fn temporary_store_names(&self) -> Result<Vec<String>> {
            self.stores()
        }

        async fn create(&self, name: &str, _protocol_version: u32) -> Result {
            self.record(format!("create {name}"))?;
            anyhow::ensure!(!self.refuse_create, "create refused");
            let mut stores = self
                .stores
                .lock()
                .map_err(|_| anyhow::anyhow!("poisoned"))?;
            anyhow::ensure!(stores.insert(name.to_string()), "store {name} exists");
            Ok(())
        }

        async fn drop_store(&self, name: &str) -> Result {
            self.record(format!("drop {name}"))?;
            let mut stores = self
                .stores
                .lock()
                .map_err(|_| anyhow::anyhow!("poisoned"))?;
            anyhow::ensure!(stores.remove(name), "store {name} unknown");
            Ok(())
        }
    }

    const INSTANCE: &str = "party-change-threshold-1760000000";

    #[tokio::test]
    async fn the_store_is_dropped_after_the_body_succeeds() -> Result {
        let recorder = Recorder::default();
        let used = in_store(
            recorder.clone(),
            INSTANCE,
            35,
            |store| async move { Ok(store) },
        )
        .await?;

        assert!(recorder.stores()?.is_empty(), "{:?}", recorder.stores()?);
        assert_eq!(
            recorder.calls()?,
            [format!("create {used}"), format!("drop {used}")]
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_store_is_dropped_when_the_body_fails() -> Result {
        let recorder = Recorder::default();
        let result: Result<()> = in_store(recorder.clone(), INSTANCE, 35, |_| async {
            anyhow::bail!("Canton refused to sign")
        })
        .await;

        let error = result
            .err()
            .ok_or_else(|| anyhow::anyhow!("body error was swallowed"))?;
        assert!(format!("{error}").contains("refused to sign"), "{error}");
        assert!(recorder.stores()?.is_empty(), "{:?}", recorder.stores()?);
        Ok(())
    }

    /// A cancelled workflow aborts its task mid-body, so the code after the
    /// body never runs. The guard has to drop the store on its own.
    #[tokio::test]
    async fn the_store_is_dropped_when_the_run_is_cancelled() -> Result {
        let recorder = Recorder::default();
        let created = Arc::new(Notify::new());
        let task = tokio::spawn({
            let (recorder, created) = (recorder.clone(), created.clone());
            async move {
                in_store(recorder, INSTANCE, 35, |_| async move {
                    created.notify_one();
                    std::future::pending::<Result>().await
                })
                .await
            }
        });

        created.notified().await;
        assert_eq!(recorder.stores()?.len(), 1, "the body runs with its store");
        task.abort();
        assert!(task.await.is_err_and(|e| e.is_cancelled()));

        tokio::time::timeout(Duration::from_secs(5), async {
            while !recorder.stores().map(|s| s.is_empty()).unwrap_or(false) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("the aborted run's store was never dropped"))?;
        Ok(())
    }

    /// A run that crashed while signing resumes at the same step. Its new
    /// attempt drops what the old one left, and only that.
    #[tokio::test]
    async fn opening_drops_only_what_an_earlier_attempt_of_the_run_left() -> Result {
        let earlier = store_name(INSTANCE, 1);
        let sibling = store_name("party-change-threshold-17600000001", 1);
        let foreign = "someone-elses-store";
        let recorder = Recorder::with_stores(&[&earlier, &sibling, foreign]);

        in_store(recorder.clone(), INSTANCE, 35, |_| async { Ok(()) }).await?;

        assert_eq!(
            recorder.stores()?,
            [sibling.clone(), foreign.to_string()],
            "only {earlier} may go"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_refused_create_leaves_nothing_to_drop() -> Result {
        let recorder = Recorder {
            refuse_create: true,
            ..Recorder::default()
        };
        let result = in_store(recorder.clone(), INSTANCE, 35, |_| async { Ok(()) }).await;

        assert!(result.is_err());
        assert!(
            recorder
                .calls()?
                .iter()
                .all(|call| !call.starts_with("drop")),
            "{:?}",
            recorder.calls()?
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_boot_sweep_drops_only_this_tools_stores() -> Result {
        let ours = [store_name(INSTANCE, 1), store_name("other-kick-1", 2)];
        let recorder = Recorder::with_stores(&[&ours[0], &ours[1], "an-operators-store"]);

        let dropped = drop_where(&recorder, |name| name.starts_with(STORE_PREFIX)).await?;

        assert_eq!(dropped, 2);
        assert_eq!(recorder.stores()?, ["an-operators-store".to_string()]);
        Ok(())
    }

    #[test]
    fn store_names_fit_canton_and_name_their_run() {
        let long = format!("{}-kick-1760000000", "p".repeat(400));
        let name = store_name(&long, u128::MAX);
        assert!(name.len() <= 185, "{} characters", name.len());
        assert!(name.starts_with(STORE_PREFIX));

        let odd = store_name("party with spaces/and::colons", 7);
        assert_eq!(
            odd,
            format!("{STORE_PREFIX}party-with-spaces-and--colons~7")
        );
    }

    /// The separator keeps one run's prefix from matching a run whose
    /// instance name merely starts the same way.
    #[test]
    fn a_runs_prefix_does_not_match_a_longer_instance_name() {
        let prefix = instance_prefix("party-kick-1");
        assert!(store_name("party-kick-1", 5).starts_with(&prefix));
        assert!(!store_name("party-kick-12", 5).starts_with(&prefix));
    }

    #[test]
    fn a_serial_at_the_ceiling_has_no_successor() -> Result {
        assert_eq!(next_serial("DNS", 4)?, 5);
        assert!(next_serial("DNS", u32::MAX).is_err());
        Ok(())
    }
}
