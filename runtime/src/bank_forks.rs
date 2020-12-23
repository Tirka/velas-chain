//! The `bank_forks` module implements BankForks a DAG of checkpointed Banks

use crate::snapshot_package::{AccountsPackageSendError, AccountsPackageSender};
use crate::snapshot_utils::{self, SnapshotError};
use crate::{bank::Bank, status_cache::MAX_CACHE_ENTRIES};
use log::*;
use solana_measure::measure::Measure;
use solana_metrics::inc_new_counter_info;
use solana_sdk::{clock::Slot, timing};
use std::{
    collections::{HashMap, HashSet},
    ops::Index,
    path::PathBuf,
    sync::Arc,
    time::Instant,
};
use thiserror::Error;

pub use crate::snapshot_utils::SnapshotVersion;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompressionType {
    Bzip2,
    Gzip,
    Zstd,
    NoCompression,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotConfig {
    // Generate a new snapshot every this many slots
    pub snapshot_interval_slots: u64,

    // Where to store the latest packaged snapshot
    pub snapshot_package_output_path: PathBuf,

    // Where to place the snapshots for recent slots
    pub snapshot_path: PathBuf,

    pub compression: CompressionType,

    // Snapshot version to generate
    pub snapshot_version: SnapshotVersion,
}

#[derive(Error, Debug)]
pub enum BankForksError {
    #[error("snapshot error")]
    SnapshotError(#[from] SnapshotError),

    #[error("accounts package send error")]
    AccountsPackageSendError(#[from] AccountsPackageSendError),
}
type Result<T> = std::result::Result<T, BankForksError>;

pub struct BankForks {
    pub banks: HashMap<Slot, Arc<Bank>>,
    root: Slot,
    pub snapshot_config: Option<SnapshotConfig>,
    last_snapshot_slot: Slot,

    pub accounts_hash_interval_slots: Slot,
    last_accounts_hash_slot: Slot,
}

impl Index<u64> for BankForks {
    type Output = Arc<Bank>;
    fn index(&self, bank_slot: Slot) -> &Self::Output {
        &self.banks[&bank_slot]
    }
}

impl BankForks {
    pub fn new(bank: Bank) -> Self {
        let root = bank.slot();
        Self::new_from_banks(&[Arc::new(bank)], root)
    }

    /// Create a map of bank slot id to the set of ancestors for the bank slot.
    pub fn ancestors(&self) -> HashMap<Slot, HashSet<Slot>> {
        let mut ancestors = HashMap::new();
        let root = self.root;
        for bank in self.banks.values() {
            let mut set: HashSet<Slot> = bank
                .ancestors
                .keys()
                .filter(|k| **k >= root)
                .cloned()
                .collect();
            set.remove(&bank.slot());
            ancestors.insert(bank.slot(), set);
        }
        ancestors
    }

    /// Create a map of bank slot id to the set of all of its descendants
    #[allow(clippy::or_fun_call)]
    pub fn descendants(&self) -> HashMap<Slot, HashSet<Slot>> {
        let mut descendants = HashMap::new();
        for bank in self.banks.values() {
            let _ = descendants.entry(bank.slot()).or_insert(HashSet::new());
            let mut set: HashSet<Slot> = bank.ancestors.keys().cloned().collect();
            set.remove(&bank.slot());
            for parent in set {
                descendants
                    .entry(parent)
                    .or_insert(HashSet::new())
                    .insert(bank.slot());
            }
        }
        descendants
    }

    pub fn frozen_banks(&self) -> HashMap<Slot, Arc<Bank>> {
        self.banks
            .iter()
            .filter(|(_, b)| b.is_frozen())
            .map(|(k, b)| (*k, b.clone()))
            .collect()
    }

    pub fn active_banks(&self) -> Vec<Slot> {
        self.banks
            .iter()
            .filter(|(_, v)| !v.is_frozen())
            .map(|(k, _v)| *k)
            .collect()
    }

    pub fn get(&self, bank_slot: Slot) -> Option<&Arc<Bank>> {
        self.banks.get(&bank_slot)
    }

    pub fn root_bank(&self) -> &Arc<Bank> {
        &self[self.root()]
    }

    pub fn new_from_banks(initial_forks: &[Arc<Bank>], root: Slot) -> Self {
        let mut banks = HashMap::new();

        // Iterate through the heads of all the different forks
        for bank in initial_forks {
            banks.insert(bank.slot(), bank.clone());
            let parents = bank.parents();
            for parent in parents {
                if banks.contains_key(&parent.slot()) {
                    // All ancestors have already been inserted by another fork
                    break;
                }
                banks.insert(parent.slot(), parent.clone());
            }
        }

        Self {
            root,
            banks,
            snapshot_config: None,
            last_snapshot_slot: root,
            accounts_hash_interval_slots: std::u64::MAX,
            last_accounts_hash_slot: root,
        }
    }

    pub fn insert(&mut self, bank: Bank) -> Arc<Bank> {
        let bank = Arc::new(bank);
        let prev = self.banks.insert(bank.slot(), bank.clone());
        assert!(prev.is_none());
        bank
    }

    pub fn remove(&mut self, slot: Slot) -> Option<Arc<Bank>> {
        self.banks.remove(&slot)
    }

    pub fn highest_slot(&self) -> Slot {
        self.banks.values().map(|bank| bank.slot()).max().unwrap()
    }

    pub fn working_bank(&self) -> Arc<Bank> {
        self[self.highest_slot()].clone()
    }

    pub fn set_root(
        &mut self,
        root: Slot,
        accounts_package_sender: &Option<AccountsPackageSender>,
        highest_confirmed_root: Option<Slot>,
    ) {
        let old_epoch = self.root_bank().epoch();
        self.root = root;
        let set_root_start = Instant::now();
        let root_bank = self
            .banks
            .get(&root)
            .expect("root bank didn't exist in bank_forks");
        let new_epoch = root_bank.epoch();
        if old_epoch != new_epoch {
            info!(
                "Root entering
                epoch: {},
                next_epoch_start_slot: {},
                epoch_stakes: {:#?}",
                new_epoch,
                root_bank
                    .epoch_schedule()
                    .get_first_slot_in_epoch(new_epoch + 1),
                root_bank
                    .epoch_stakes(new_epoch)
                    .unwrap()
                    .node_id_to_vote_accounts()
            );
        }
        let root_tx_count = root_bank
            .parents()
            .last()
            .map(|bank| bank.transaction_count())
            .unwrap_or(0);
        // Calculate the accounts hash at a fixed interval
        let mut is_root_bank_squashed = false;
        let mut banks = vec![root_bank];
        let parents = root_bank.parents();
        banks.extend(parents.iter());
        for bank in banks.iter() {
            // bank.evm_state
            //     .write()
            //     .expect("evm state was poisoned")
            //     .dump_all()
            //     .expect("internal evm state error");

            let bank_slot = bank.slot();
            if bank.block_height() % self.accounts_hash_interval_slots == 0
                && bank_slot > self.last_accounts_hash_slot
            {
                self.last_accounts_hash_slot = bank_slot;
                bank.squash();
                is_root_bank_squashed = bank_slot == root;

                bank.update_accounts_hash();

                if self.snapshot_config.is_some() && accounts_package_sender.is_some() {
                    // Generate an accounts package
                    let mut snapshot_time = Measure::start("total-snapshot-ms");
                    let r = self.generate_accounts_package(
                        bank_slot,
                        &bank.src.roots(),
                        accounts_package_sender.as_ref().unwrap(),
                    );
                    if r.is_err() {
                        warn!(
                            "Error generating snapshot for bank: {}, err: {:?}",
                            bank_slot, r
                        );
                    } else {
                        self.last_snapshot_slot = bank_slot;
                    }

                    // Cleanup outdated snapshots
                    self.purge_old_snapshots();
                    snapshot_time.stop();
                    inc_new_counter_info!("total-snapshot-ms", snapshot_time.as_ms() as usize);
                }
                break;
            }
        }
        if !is_root_bank_squashed {
            root_bank.squash();
        }
        let new_tx_count = root_bank.transaction_count();

        self.prune_non_root(root, highest_confirmed_root);

        inc_new_counter_info!(
            "bank-forks_set_root_ms",
            timing::duration_as_ms(&set_root_start.elapsed()) as usize
        );
        inc_new_counter_info!(
            "bank-forks_set_root_tx_count",
            (new_tx_count - root_tx_count) as usize
        );
    }

    pub fn root(&self) -> Slot {
        self.root
    }

    pub fn purge_old_snapshots(&self) {
        // Remove outdated snapshots
        let config = self.snapshot_config.as_ref().unwrap();
        let slot_snapshot_paths = snapshot_utils::get_snapshot_paths(&config.snapshot_path);
        let num_to_remove = slot_snapshot_paths.len().saturating_sub(MAX_CACHE_ENTRIES);
        for slot_files in &slot_snapshot_paths[..num_to_remove] {
            let r = snapshot_utils::remove_snapshot(slot_files.slot, &config.snapshot_path);
            if r.is_err() {
                warn!("Couldn't remove snapshot at: {:?}", config.snapshot_path);
            }
        }
    }

    pub fn generate_accounts_package(
        &self,
        root: Slot,
        slots_to_snapshot: &[Slot],
        accounts_package_sender: &AccountsPackageSender,
    ) -> Result<()> {
        let config = self.snapshot_config.as_ref().unwrap();

        // Add a snapshot for the new root
        let bank = self
            .get(root)
            .cloned()
            .expect("root must exist in BankForks");

        let storages: Vec<_> = bank.get_snapshot_storages();
        let mut add_snapshot_time = Measure::start("add-snapshot-ms");
        snapshot_utils::add_snapshot(
            &config.snapshot_path,
            &bank,
            &storages,
            config.snapshot_version,
        )?;
        add_snapshot_time.stop();
        inc_new_counter_info!("add-snapshot-ms", add_snapshot_time.as_ms() as usize);

        // Package the relevant snapshots
        let slot_snapshot_paths = snapshot_utils::get_snapshot_paths(&config.snapshot_path);
        let latest_slot_snapshot_paths = slot_snapshot_paths
            .last()
            .expect("no snapshots found in config snapshot_path");
        // We only care about the last bank's snapshot.
        // We'll ask the bank for MAX_CACHE_ENTRIES (on the rooted path) worth of statuses
        let package = snapshot_utils::package_snapshot(
            &bank,
            latest_slot_snapshot_paths,
            &config.snapshot_path,
            slots_to_snapshot,
            &config.snapshot_package_output_path,
            storages,
            config.compression.clone(),
            config.snapshot_version,
        )?;

        accounts_package_sender.send(package)?;

        Ok(())
    }

    fn prune_non_root(&mut self, root: Slot, highest_confirmed_root: Option<Slot>) {
        let descendants = self.descendants();
        self.banks.retain(|slot, _| {
            *slot == root
                || descendants[&root].contains(slot)
                || (*slot < root
                    && *slot >= highest_confirmed_root.unwrap_or(root)
                    && descendants[slot].contains(&root))
        });
        datapoint_debug!(
            "bank_forks_purge_non_root",
            ("num_banks_retained", self.banks.len(), i64),
        );
    }

    pub fn set_snapshot_config(&mut self, snapshot_config: Option<SnapshotConfig>) {
        self.snapshot_config = snapshot_config;
    }

    pub fn snapshot_config(&self) -> &Option<SnapshotConfig> {
        &self.snapshot_config
    }

    pub fn set_accounts_hash_interval_slots(&mut self, accounts_interval_slots: u64) {
        self.accounts_hash_interval_slots = accounts_interval_slots;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::genesis_utils::{create_genesis_config, GenesisConfigInfo};
    use solana_sdk::hash::Hash;
    use solana_sdk::pubkey::Pubkey;

    #[test]
    fn test_bank_forks_new() {
        let GenesisConfigInfo { genesis_config, .. } = create_genesis_config(10_000);
        let bank = Bank::new(&genesis_config);
        let mut bank_forks = BankForks::new(bank);
        let child_bank = Bank::new_from_parent(&bank_forks[0u64], &Pubkey::default(), 1);
        child_bank.register_tick(&Hash::default());
        bank_forks.insert(child_bank);
        assert_eq!(bank_forks[1u64].tick_height(), 1);
        assert_eq!(bank_forks.working_bank().tick_height(), 1);
    }

    #[test]
    fn test_bank_forks_new_from_banks() {
        let GenesisConfigInfo { genesis_config, .. } = create_genesis_config(10_000);
        let bank = Arc::new(Bank::new(&genesis_config));
        let child_bank = Arc::new(Bank::new_from_parent(&bank, &Pubkey::default(), 1));

        let bank_forks = BankForks::new_from_banks(&[bank.clone(), child_bank.clone()], 0);
        assert_eq!(bank_forks.root(), 0);
        assert_eq!(bank_forks.working_bank().slot(), 1);

        let bank_forks = BankForks::new_from_banks(&[child_bank, bank], 0);
        assert_eq!(bank_forks.root(), 0);
        assert_eq!(bank_forks.working_bank().slot(), 1);
    }

    #[test]
    fn test_bank_forks_descendants() {
        let GenesisConfigInfo { genesis_config, .. } = create_genesis_config(10_000);
        let bank = Bank::new(&genesis_config);
        let mut bank_forks = BankForks::new(bank);
        let bank0 = bank_forks[0].clone();
        let bank = Bank::new_from_parent(&bank0, &Pubkey::default(), 1);
        bank_forks.insert(bank);
        let bank = Bank::new_from_parent(&bank0, &Pubkey::default(), 2);
        bank_forks.insert(bank);
        let descendants = bank_forks.descendants();
        let children: HashSet<u64> = [1u64, 2u64].to_vec().into_iter().collect();
        assert_eq!(children, *descendants.get(&0).unwrap());
        assert!(descendants[&1].is_empty());
        assert!(descendants[&2].is_empty());
    }

    #[test]
    fn test_bank_forks_ancestors() {
        let GenesisConfigInfo { genesis_config, .. } = create_genesis_config(10_000);
        let bank = Bank::new(&genesis_config);
        let mut bank_forks = BankForks::new(bank);
        let bank0 = bank_forks[0].clone();
        let bank = Bank::new_from_parent(&bank0, &Pubkey::default(), 1);
        bank_forks.insert(bank);
        let bank = Bank::new_from_parent(&bank0, &Pubkey::default(), 2);
        bank_forks.insert(bank);
        let ancestors = bank_forks.ancestors();
        assert!(ancestors[&0].is_empty());
        let parents: Vec<u64> = ancestors[&1].iter().cloned().collect();
        assert_eq!(parents, vec![0]);
        let parents: Vec<u64> = ancestors[&2].iter().cloned().collect();
        assert_eq!(parents, vec![0]);
    }

    #[test]
    fn test_bank_forks_frozen_banks() {
        let GenesisConfigInfo { genesis_config, .. } = create_genesis_config(10_000);
        let bank = Bank::new(&genesis_config);
        let mut bank_forks = BankForks::new(bank);
        let child_bank = Bank::new_from_parent(&bank_forks[0u64], &Pubkey::default(), 1);
        bank_forks.insert(child_bank);
        assert!(bank_forks.frozen_banks().get(&0).is_some());
        assert!(bank_forks.frozen_banks().get(&1).is_none());
    }

    #[test]
    fn test_bank_forks_active_banks() {
        let GenesisConfigInfo { genesis_config, .. } = create_genesis_config(10_000);
        let bank = Bank::new(&genesis_config);
        let mut bank_forks = BankForks::new(bank);
        let child_bank = Bank::new_from_parent(&bank_forks[0u64], &Pubkey::default(), 1);
        bank_forks.insert(child_bank);
        assert_eq!(bank_forks.active_banks(), vec![1]);
    }
}
