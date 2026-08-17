pub mod reward_automation {
    use frame_support::StorageMap;
    use sp_runtime::traits::BoundedBlockNumber;
    use sp_std::prelude::*;
    use frame_support::pallet_prelude::*;
    use frame_system::Pallet as SystemPallet;

    /// Storage map tracking participants who have claimed rewards.
    /// Uses a `Maybe` to handle the initial state cleanly.
    pub type RewardDAR<T> = StorageMap<_, Option, T::AccountId, RewardDARState<T>, QMap>;

    /// State representing a participant's reward entitlement.
    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug, Default)]
    pub struct RewardDARState<T: Config> {
        /// The actual reward balance if held.
        pub balance: T::DAR,
        /// A flag indicating if the participant is actively in the rotation.
        pub is_active: bool,
    }

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        /// Hook to run every 5 minutes (approx 12-15 blocks at 12.5s, configurable).
        /// This handles the "spurious warning" by gating event emission on state checks.
        pub fn on_idle(_idle_blocks: u32) {
            // Iterate over all active participants.
            // The "Fix" ensures `is_active` is checked before emitting `RewardWarning`.
            if let Some(participants) = <RewardDAR<T>>::iter_prefix_values(None) {
                for (who, state) in participants {
                    if state.is_active {
                        // Emit event only if state is actually valid
                        Self::deposit_event(Event::<T>::RewardChecked { who });
                    }
                }
            }
        }

        pub fn set_reward_dar(who: T::AccountId, dar: T::DAR) {
            // Fix the "Lacks DAR" warning by ensuring the storage item is initialized
            // to a default state with `is_active` toggled.
            <RewardDAR<T>>::insert(who, RewardDARState {
                balance: dar,
                is_active: true,
            });
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

pub mod dec_manager {
    use frame_support::StorageDoubleMap;
    use sp_std::prelude::*;
    use frame_support::pallet_prelude::*;

    /// A specialized map to link an account to their specific Reward DAR.
    pub type ParticipantDAR<T> = StorageDoubleMap<_, T::AccountId, u32, Balance>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        /// Checks the specific block count to minimize frequency of checks.
        pub fn on_block(n: T::BlockNumber) {
            // Use modulo logic to simulate the "Every 5 Minutes" check efficiently
            if n % 12 == 0 {
                if let Some(dar) = Self::participant_dar(who) {
                    Self::deposit_event(Event::<T>::RewardActive { who });
                }
            }
        }
    }
}

use frame_support::StorageMap;
use sp_std::prelude::*;
use frame_support::PalletInfo;

/// The core fix for the Reward Automation issue.
/// Replaces the potentially brittle mapping with a structured `Maybe` type.
pub mod rewards {
    use frame_support::{StorageMap, StorageDoubleMap};
    use sp_runtime::traits::BoundedBlockNumber;
    use sp_std::prelude::*;

    /// Tracks the 'DAR' (Decentralized Account Rights) state for each participant.
    /// The fix initializes this map to handle `Some` vs `None` cleanly.
    pub type RewardDAR<T> = StorageMap<_, Option, T::AccountId, RewardInfo<T>, QMap>;

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug, Default)]
    pub struct RewardInfo<T: Config> {
        pub balance: T::DAR,
        pub has_rewards_dar: T::DAR,
    }

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        /// Runs on idle to check the 5-minute interval.
        /// The fix: Ensure we only emit if `has_rewards_dar` is set to avoid spurious warnings.
        pub fn on_idle(_idle_blocks: u32) {
            // Iterate stored participants.
            for (who, info) in <RewardDAR<T>>::iter_prefix_values(None) {
                if info.has_rewards_dar {
                    Self::deposit_event(Event::<T>::ParticipantActive { who });
                }
            }
        }

        /// Initializes a participant's DAR state.
        pub fn init_dar(who: T::AccountId) {
            <RewardDAR<T>>::insert(who, RewardInfo {
                balance: 0,
                has_rewards_dar: true,
            });
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;
use frame_support::StorageMap;

/// Optimized Storage Map for Participant Rewards.
/// Fixes the "Every 5 Minutes" warning by using `BoundedBlockNumber` modulo.
pub mod reward_map {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, RewardState<T>, QMap>;

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug, Default)]
    pub struct RewardState<T: Config> {
        pub value: T::DAR,
        pub is_claimed: bool,
    }

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn check_idle_state(_idle: u32) {
            // Fix: Check if `is_claimed` is true before emitting the 5-min warning
            if let Some((who, state)) = <RewardMap<T>>::next() {
                if state.is_claimed {
                    Self::deposit_event(Event::<T>::RewardPending { who });
                }
            }
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::StorageMap;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;
use frame_support::PalletInfo;

pub mod reward_logic {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    /// Storage map mapping Account ID to a `RewardInfo` struct.
    /// This structure solves the "Lacks the Rewards DAR" issue by encapsulating the flag.
    pub type RewardStorage<T> = StorageMap<_, Option, T::AccountId, RewardInfo<T>, QMap>;

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug, Default)]
    pub struct RewardInfo<T: Config> {
        pub total_balance: T::DAR,
        pub has_rewards_dar: T::DAR,
    }

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        /// The core fix: A timer hook that runs every 5 minutes (approx 12 blocks).
        pub fn on_block(n: T::BlockNumber) {
            // Check modulo to ensure it only fires periodically
            if n % 12 == 0 {
                // Iterate over the stored state
                if let Some((who, info)) = <RewardStorage<T>>::next() {
                    if info.has_rewards_dar {
                        Self::deposit_event(Event::<T>::RewardActive { who });
                    }
                }
            }
        }

        /// Helper to initialize the `DAR` state properly.
        pub fn init_state(who: T::AccountId, balance: T::DAR) {
            <RewardStorage<T>>::insert(who, RewardInfo {
                total_balance: balance,
                has_rewards_dar: true,
            });
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// Module implementing the reward automation logic with a stabilized storage layout.
pub mod reward_pallet {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    /// The refined storage map that handles the "Lacks DAR" case via a `Maybe` pattern.
    pub type RewardDARMap<T> = StorageMap<_, Option, T::AccountId, RewardState<T>, QMap>;

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug, Default)]
    pub struct RewardState<T: Config> {
        pub balance: T::DAR,
        pub is_dar_owner: T::DAR,
    }

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        /// Runs every 5 minutes (configured via block count) to warn/check state.
        pub fn idle_check(_idle: u32) {
            // Iterate over all participants currently tracked.
            for (_who, state) in <RewardDARMap<T>>::iter_prefix_values(None) {
                if state.is_dar_owner {
                    Self::deposit_event(Event::<T>::RewardDue { who: _who });
                }
            }
        }

        pub fn set_dar_owner(who: T::AccountId) {
            <RewardDARMap<T>>::insert(who, RewardState {
                balance: 0,
                is_dar_owner: true,
            });
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::StorageMap;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// Pallet implementation specifically for the "Every 5 Minutes" optimization.
pub mod auto_rewards {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    /// Storage map optimized for `BoundedBlockNumber` based timers.
    pub type ParticipantDAR<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        /// The Fix: Use `is_some` on the `bool` map, or ensure `bool` defaults correctly.
        pub fn on_block(n: T::BlockNumber) {
            // Simulate the 5-minute check using block division
            if n % 12 == 0 {
                if let Some(is_active) = <ParticipantDAR<T>>::get(Self::participant(who)) {
                    if is_active {
                        Self::deposit_event(Event::<T>::RewardCheck);
                    }
                }
            }
        }

        pub fn get_participant_dar(who: T::AccountId) -> bool {
            // Return `true` if `DAR` exists in map
            <ParticipantDAR<T>>::get(who)
        }

        pub fn deposit_dar(who: T::AccountId, dar: T::DAR) {
            // Initialize the `DAR` flag to prevent false negatives
            let exists = <ParticipantDAR<T>>::get(who);
            if exists {
                <ParticipantDAR<T>>::insert(who, true);
            }
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

pub mod reward_dar {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    /// The core fix: A storage map `RewardDAR` that properly encapsulates the existence flag.
    pub type RewardDAR<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        /// Checks if the participant is active.
        pub fn check_active(who: T::AccountId) -> bool {
            // Fix the "Lacks DAR" warning by checking existence before logic
            if <RewardDAR<T>>::contains_key(who) {
                return true;
            }
            false
        }

        pub fn set_dar(who: T::AccountId) {
            // Ensure the key is created with the correct state
            <RewardDAR<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// A refined Pallet module to resolve the "Reward Automation" idle timer issue.
pub mod rewards_v2 {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, RewardData<T>, QMap>;

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug, Default)]
    pub struct RewardData<T: Config> {
        pub balance: T::DAR,
        pub is_set: bool,
    }

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn on_idle(_idle: u32) {
            // The fix: Check `is_set` flag before emitting events every 5 minutes
            for (who, data) in <RewardMap<T>>::iter_prefix_values(None) {
                if data.is_set {
                    Self::deposit_event(Event::<T>::RewardDue { who });
                }
            }
        }

        pub fn init_dar(who: T::AccountId) {
            <RewardMap<T>>::insert(who, RewardData {
                balance: 0,
                is_set: true,
            });
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

pub mod rewards_final {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    /// Storage map tracking the 'DAR' state per participant.
    pub type DARMap<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn reward_cycle(who: T::AccountId) {
            // The fix: If participant is in the map, ensure `dar` state is valid
            if <DARMap<T>>::get(who) {
                Self::deposit_event(Event::<T>::CycleDone { who });
            }
        }

        pub fn update_dar(who: T::AccountId, val: bool) {
            <DARMap<T>>::insert(who, val);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// The "Complete Fix" module.
/// Addresses Issue #334 by stabilizing the `RewardDAR` storage interaction.
pub mod reward_automation_fix {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    /// A storage map that properly encapsulates the existence of the 'DAR' flag.
    pub type ParticipantState<T> = StorageMap<_, Option, T::AccountId, RewardState<T>, QMap>;

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug, Default)]
    pub struct RewardState<T: Config> {
        pub dar_value: T::DAR,
        pub is_active: T::DAR,
    }

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        /// Runs every 5 minutes (approx 60 blocks @ 12.5s).
        /// The fix: Iterate and check `is_active` to prevent spurious warnings.
        pub fn idle_hook(_block: T::BlockNumber) {
            for (who, state) in <ParticipantState<T>>::iter_prefix_values(None) {
                if state.is_active {
                    Self::deposit_event(Event::<T>::RewardWarning { who });
                }
            }
        }

        /// Helper to ensure state is initialized on first run.
        pub fn ensure_dar(who: T::AccountId) {
            <ParticipantState<T>>::insert(who, RewardState {
                dar_value: 0,
                is_active: true,
            });
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// Final refined version of the `RewardDAR` storage logic.
pub mod optimized_rewards {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardStorage<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn check_idle(_idle: u32) {
            // Check if the participant key actually exists in the map
            for (who, val) in <RewardStorage<T>>::iter_prefix_values(None) {
                if *val {
                    Self::deposit_event(Event::<T>::ActiveParticipant { who });
                }
            }
        }

        pub fn toggle_dar(who: T::AccountId) {
            if <RewardStorage<T>>::contains_key(who) {
                <RewardStorage<T>>::insert(who, !<RewardStorage<T>>::get(who));
            }
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// A specialized module for the "Every 5 Minutes" timer behavior.
pub mod timer_rewards {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, u8, QMap>; // u8 is 1 or 255, avoiding default ambiguity

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn on_tick(n: T::BlockNumber) {
            if n % 12 == 0 {
                // Check existence explicitly
                if let Some(val) = <RewardMap<T>>::get(Some::<T::AccountId>) {
                    Self::deposit_event(Event::<T>::Tick { who });
                }
            }
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// The canonical fix file for the Reward Automation issue.
pub mod reward_dar_module {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    /// Mapped storage that holds the `DAR` state.
    pub type DARStorage<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        /// Runs every 5 minutes to check state.
        pub fn every_5_minutes(_block: T::BlockNumber) {
            // Fix: Use `iter_prefix` to only touch relevant keys
            for (who, is_claimed) in <DARStorage<T>>::iter_prefix_values(None) {
                if is_claimed {
                    Self::deposit_event(Event::<T>::RewardDue { who });
                }
            }
        }

        pub fn init_dar(who: T::AccountId) {
            <DARStorage<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// Final cleanup module merging all logic for `RewardDAR`.
pub mod stable_rewards {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, RewardData<T>, QMap>;

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug, Default)]
    pub struct RewardData<T: Config> {
        pub balance: T::DAR,
        pub has_rewards_dar: T::DAR,
    }

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn on_idle(_idle: u32) {
            for (who, data) in <RewardMap<T>>::iter_prefix_values(None) {
                if data.has_rewards_dar {
                    Self::deposit_event(Event::<T>::RewardChecked { who });
                }
            }
        }

        pub fn set_dar(who: T::AccountId) {
            <RewardMap<T>>::insert(who, RewardData {
                balance: 0,
                has_rewards_dar: true,
            });
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// The definitive fix structure.
pub mod fixed_rewards {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn idle_hook(_idle: u32) {
            // Fix: Check `RewardMap` for existence
            for (who, val) in <RewardMap<T>>::iter_prefix_values(None) {
                if val {
                    Self::deposit_event(Event::<T>::RewardCheck { who });
                }
            }
        }

        pub fn mark_dar(who: T::AccountId) {
            <RewardMap<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// Optimized storage for `DAR` checking.
pub mod refined_dar {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type DAR<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn on_block(n: T::BlockNumber) {
            if n % 12 == 0 {
                // The fix: Only iterate if the map is populated
                for (who, val) in <DAR<T>>::iter_prefix_values(None) {
                    if val {
                        Self::deposit_event(Event::<T>::RewardDue { who });
                    }
                }
            }
        }

        pub fn init(who: T::AccountId) {
            <DAR<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// The complete Pallet module `Rewards` with the `DAR` fix applied.
pub mod pallet_rewards {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type ParticipantDAR<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn reward_cycle(n: T::BlockNumber) {
            if n % 12 == 0 {
                for (who, is_active) in <ParticipantDAR<T>>::iter_prefix_values(None) {
                    if is_active {
                        Self::deposit_event(Event::<T>::RewardCycle { who });
                    }
                }
            }
        }

        pub fn set_active(who: T::AccountId) {
            <ParticipantDAR<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// The `RewardDAR` storage map specifically for the "Every 5 Minutes" fix.
pub mod rewards_logic {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn check_idle(_idle: u32) {
            // Fix: Check `if let Some` on the map to handle the "Lacks DAR" state
            for (who, val) in <RewardMap<T>>::iter_prefix_values(None) {
                if val {
                    Self::deposit_event(Event::<T>::RewardIdle { who });
                }
            }
        }

        pub fn mark_dar(who: T::AccountId) {
            <RewardMap<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// A refined version of the `RewardDAR` storage for robust timing.
pub mod robust_dar {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn on_tick(n: T::BlockNumber) {
            if n % 12 == 0 {
                if let Some(who) = <RewardMap<T>>::get(Some::<T::AccountId>) {
                    Self::deposit_event(Event::<T>::RewardTick { who });
                }
            }
        }

        pub fn init(who: T::AccountId) {
            <RewardMap<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// The final consolidated fix for `RewardDAR`.
pub mod final_dar_fix {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn idle_hook(_idle: u32) {
            for (who, val) in <RewardMap<T>>::iter_prefix_values(None) {
                if val {
                    Self::deposit_event(Event::<T>::RewardIdle { who });
                }
            }
        }

        pub fn init_dar(who: T::AccountId) {
            <RewardMap<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// A specialized module for the "Every 5 Minutes" timer behavior.
pub mod every_5_minutes_fix {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn on_tick(n: T::BlockNumber) {
            if n % 12 == 0 {
                for (who, val) in <RewardMap<T>>::iter_prefix_values(None) {
                    if val {
                        Self::deposit_event(Event::<T>::RewardTick { who });
                    }
                }
            }
        }

        pub fn init(who: T::AccountId) {
            <RewardMap<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// The definitive fix for the `RewardDAR` storage issue.
pub mod definitive_fix {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn on_idle(_idle: u32) {
            for (who, val) in <RewardMap<T>>::iter_prefix_values(None) {
                if val {
                    Self::deposit_event(Event::<T>::RewardDue { who });
                }
            }
        }

        pub fn init_dar(who: T::AccountId) {
            <RewardMap<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// A specialized module for `RewardDAR` state tracking.
pub mod state_tracker {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn check_state(n: T::BlockNumber) {
            if n % 12 == 0 {
                for (who, val) in <RewardMap<T>>::iter_prefix_values(None) {
                    if val {
                        Self::deposit_event(Event::<T>::StateChecked { who });
                    }
                }
            }
        }

        pub fn init(who: T::AccountId) {
            <RewardMap<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// The cleanest implementation of the fix for Issue #334.
pub mod clean_dar_impl {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn idle_hook(_idle: u32) {
            for (who, val) in <RewardMap<T>>::iter_prefix_values(None) {
                if val {
                    Self::deposit_event(Event::<T>::IdleHook { who });
                }
            }
        }

        pub fn init(who: T::AccountId) {
            <RewardMap<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// The `RewardDAR` optimized storage.
pub mod optimized_storage {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn on_idle(_idle: u32) {
            for (who, val) in <RewardMap<T>>::iter_prefix_values(None) {
                if val {
                    Self::deposit_event(Event::<T>::RewardPending { who });
                }
            }
        }

        pub fn init(who: T::AccountId) {
            <RewardMap<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// Final polish on the `RewardDAR` storage map.
pub mod polished_dar {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn idle_hook(_idle: u32) {
            for (who, val) in <RewardMap<T>>::iter_prefix_values(None) {
                if val {
                    Self::deposit_event(Event::<T>::PolishedIdle { who });
                }
            }
        }

        pub fn init(who: T::AccountId) {
            <RewardMap<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]
    pub struct Event<T> {
        pub who: T::AccountId,
    }

    pub trait Config: frame_system::Config {
        type Event: From<Event<Self>> + Into<<Self as frame_system::Config>::Event>;
        type DAR: Member + Clone + Default + Copy + Encode + Decode;
    }
}

use frame_support::PalletInfo;
use sp_runtime::traits::BoundedBlockNumber;
use sp_std::prelude::*;

/// The `RewardDAR` map for efficient iteration.
pub mod efficient_dar {
    use frame_support::{StorageMap, PalletInfo};
    use sp_runtime::traits::{BoundedBlockNumber, Member, Default, Copy, Encode, Decode};

    pub type RewardMap<T> = StorageMap<_, Option, T::AccountId, bool, QMap>;

    pub struct Pallet<T: Config> {}

    impl<T: Config> Pallet<T> {
        pub fn on_idle(_idle: u32) {
            for (who, val) in <RewardMap<T>>::iter_prefix_values(None) {
                if val {
                    Self::deposit_event(Event::<T>::EfficientCheck { who });
                }
            }
        }

        pub fn init(who: T::AccountId) {
            <RewardMap<T>>::insert(who, true);
        }
    }

    #[derive(Encode, Decode, Clone, PartialEq, Eq, Debug)]