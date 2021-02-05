use crate::leader_schedule::LeaderSchedule;
use solana_runtime::bank::Bank;
use solana_sdk::{
    clock::{Epoch, Slot, NUM_CONSECUTIVE_LEADER_SLOTS},
    pubkey::Pubkey,
};
use std::collections::HashMap;

pub use solana_stake_program::stake_state::{
    MIN_STAKERS_TO_BE_MAJORITY, NUM_MAJOR_STAKERS_FOR_FILTERING,
};

/// Return the leader schedule for the given epoch.
pub fn leader_schedule(epoch: Epoch, bank: &Bank) -> Option<LeaderSchedule> {
    bank.epoch_staked_nodes(epoch).map(|stakes| {
        let mut seed = [0u8; 32];
        seed[0..8].copy_from_slice(&epoch.to_le_bytes());
        let stakes = retain_sort_stakers(stakes);
        LeaderSchedule::new(
            &stakes,
            seed,
            bank.get_slots_in_epoch(epoch),
            NUM_CONSECUTIVE_LEADER_SLOTS,
        )
    })
}

fn retain_sort_stakers(stakes: HashMap<Pubkey, u64>) -> Vec<(Pubkey, u64)> {
    let mut stakes: Vec<_> = stakes.into_iter().collect();
    sort_stakes(&mut stakes);
    if num_major_stakers(&stakes) >= NUM_MAJOR_STAKERS_FOR_FILTERING {
        retain_major_stakers(&mut stakes)
    }
    stakes
}

/// Return the leader for the given slot.
pub fn slot_leader_at(slot: Slot, bank: &Bank) -> Option<Pubkey> {
    let (epoch, slot_index) = bank.get_epoch_and_slot_index(slot);

    leader_schedule(epoch, bank).map(|leader_schedule| leader_schedule[slot_index])
}

// Returns the number of ticks remaining from the specified tick_height to the end of the
// slot implied by the tick_height
pub fn num_ticks_left_in_slot(bank: &Bank, tick_height: u64) -> u64 {
    bank.ticks_per_slot() - tick_height % bank.ticks_per_slot()
}

fn sort_stakes(stakes: &mut Vec<(Pubkey, u64)>) {
    // Sort first by stake. If stakes are the same, sort by pubkey to ensure a
    // deterministic result.
    // Note: Use unstable sort, because we dedup right after to remove the equal elements.
    stakes.sort_unstable_by(|(l_pubkey, l_stake), (r_pubkey, r_stake)| {
        if r_stake == l_stake {
            r_pubkey.cmp(&l_pubkey)
        } else {
            r_stake.cmp(&l_stake)
        }
    });

    // Now that it's sorted, we can do an O(n) dedup.
    stakes.dedup();
}

fn num_major_stakers(stakes: &Vec<(Pubkey, u64)>) -> usize {
    stakes
        .iter()
        .filter(|s| s.1 >= MIN_STAKERS_TO_BE_MAJORITY)
        .count()
}

fn retain_major_stakers(stakes: &mut Vec<(Pubkey, u64)>) {
    stakes.retain(|s| s.1 >= MIN_STAKERS_TO_BE_MAJORITY);
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_runtime::genesis_utils::{
        bootstrap_validator_stake_lamports, create_genesis_config_with_leader,
    };

    #[test]
    fn test_leader_schedule_via_bank() {
        let pubkey = solana_sdk::pubkey::new_rand();
        let genesis_config =
            create_genesis_config_with_leader(0, &pubkey, bootstrap_validator_stake_lamports())
                .genesis_config;
        let bank = Bank::new(&genesis_config);

        let pubkeys_and_stakes: Vec<_> = bank.staked_nodes().into_iter().collect();
        let seed = [0u8; 32];
        let leader_schedule = LeaderSchedule::new(
            &pubkeys_and_stakes,
            seed,
            genesis_config.epoch_schedule.slots_per_epoch,
            NUM_CONSECUTIVE_LEADER_SLOTS,
        );

        assert_eq!(leader_schedule[0], pubkey);
        assert_eq!(leader_schedule[1], pubkey);
        assert_eq!(leader_schedule[2], pubkey);
    }

    #[test]
    fn test_leader_scheduler1_basic() {
        let pubkey = solana_sdk::pubkey::new_rand();
        let genesis_config =
            create_genesis_config_with_leader(42, &pubkey, bootstrap_validator_stake_lamports())
                .genesis_config;
        let bank = Bank::new(&genesis_config);
        assert_eq!(slot_leader_at(bank.slot(), &bank).unwrap(), pubkey);
    }

    #[test]
    fn test_sort_stakes_basic() {
        let pubkey0 = solana_sdk::pubkey::new_rand();
        let pubkey1 = solana_sdk::pubkey::new_rand();
        let mut stakes = vec![(pubkey0, 1), (pubkey1, 2)];
        sort_stakes(&mut stakes);
        assert_eq!(stakes, vec![(pubkey1, 2), (pubkey0, 1)]);
    }

    #[test]
    fn test_sort_stakes_with_dup() {
        let pubkey0 = solana_sdk::pubkey::new_rand();
        let pubkey1 = solana_sdk::pubkey::new_rand();
        let mut stakes = vec![(pubkey0, 1), (pubkey1, 2), (pubkey0, 1)];
        sort_stakes(&mut stakes);
        assert_eq!(stakes, vec![(pubkey1, 2), (pubkey0, 1)]);
    }

    #[test]
    fn test_sort_stakes_with_equal_stakes() {
        let pubkey0 = Pubkey::default();
        let pubkey1 = solana_sdk::pubkey::new_rand();
        let mut stakes = vec![(pubkey0, 1), (pubkey1, 1)];
        sort_stakes(&mut stakes);
        assert_eq!(stakes, vec![(pubkey1, 1), (pubkey0, 1)]);
    }

    #[test]
    fn majoirty_test() {
        let mut stakes = HashMap::new();
        // Test case without majoirty
        for _ in 0..30 {
            stakes.insert(Pubkey::new_unique(), 1);
        }
        let num_stakers = retain_sort_stakers(stakes.clone());
        assert_eq!(num_stakers.len(), 30);

        for _ in 0..30 {
            stakes.insert(Pubkey::new_unique(), MIN_STAKERS_TO_BE_MAJORITY - 1);
        }
        let num_stakers = retain_sort_stakers(stakes.clone());
        assert_eq!(num_stakers.len(), 60);

        // Test case for majoirty < NUM_MAJOR_STAKERS_FOR_FILTERING
        for _ in 0..(NUM_MAJOR_STAKERS_FOR_FILTERING - 1) {
            stakes.insert(Pubkey::new_unique(), MIN_STAKERS_TO_BE_MAJORITY);
        }
        let num_stakers = retain_sort_stakers(stakes.clone());
        assert_eq!(
            num_stakers.len(),
            60 + (NUM_MAJOR_STAKERS_FOR_FILTERING - 1)
        );

        // Test case for majoirty >= MIN_MAJOIRTY
        // Should remove all nodes without majoirty stake.

        stakes.insert(Pubkey::new_unique(), MIN_STAKERS_TO_BE_MAJORITY);
        let num_stakers = retain_sort_stakers(stakes.clone());
        assert_eq!(num_stakers.len(), NUM_MAJOR_STAKERS_FOR_FILTERING);

        // Test case where more than NUM_MAJOR_STAKERS_FOR_FILTERING, should keep all majority in stakers.
        for n in 0..30 {
            stakes.insert(Pubkey::new_unique(), MIN_STAKERS_TO_BE_MAJORITY + n * 100);
        }
        let num_stakers = retain_sort_stakers(stakes.clone());
        assert_eq!(num_stakers.len(), NUM_MAJOR_STAKERS_FOR_FILTERING + 30);
        assert_eq!(stakes.len(), NUM_MAJOR_STAKERS_FOR_FILTERING + 30 + 60)
    }
}
