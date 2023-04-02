use std::collections::HashMap;

use anchor_lang::{AnchorDeserialize, prelude::Pubkey};
use anchor_lang::Key;
use invariant_types::{
    ANCHOR_DISCRIMINATOR_SIZE,
    structs::{TICK_CROSSES_PER_IX, TICK_LIMIT, TICKMAP_SIZE}, TICK_SEED,
};
use solana_client::rpc_client::RpcClient;

use crate::JupiterInvariant;

enum PriceDirection {
    UP,
    DOWN,
}

impl JupiterInvariant {
    pub fn deserialize<T>(data: &[u8]) -> anyhow::Result<T>
    where
        T: AnchorDeserialize,
    {
        T::try_from_slice(Self::extract_from_anchor_account(data))
            .map_err(|e| anyhow::anyhow!("Error deserializing account data: {:?}", e))
    }

    pub fn fetch_accounts(
        rpc: &RpcClient,
        accounts_to_update: Vec<Pubkey>,
    ) -> HashMap<Pubkey, Vec<u8>> {
        rpc.get_multiple_accounts(&accounts_to_update)
            .unwrap()
            .iter()
            .enumerate()
            .fold(HashMap::new(), |mut m, (index, account)| {
                if let Some(account) = account {
                    m.insert(accounts_to_update[index], account.data.clone());
                }
                m
            })
    }

    pub fn tick_indexes_to_addresses(&self, indexes: &[i32]) -> Vec<Pubkey> {
        let pubkeys: Vec<Pubkey> = indexes
            .iter()
            .map(|i| self.tick_index_to_address(*i))
            .collect();
        pubkeys
    }

    pub fn tick_index_to_address(&self, i: i32) -> Pubkey {
        let (pubkey, _) = Pubkey::find_program_address(
            &[
                TICK_SEED.as_bytes(),
                self.market_key.key().as_ref(),
                &i.to_le_bytes(),
            ],
            &self.program_id,
        );
        pubkey
    }

    pub fn get_ticks_addresses_around(&self) -> Vec<Pubkey> {
        let above_indexes = self.find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::UP);
        let below_indexes =
            self.find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::DOWN);
        let all_indexes = [below_indexes, above_indexes].concat();

        self.tick_indexes_to_addresses(&all_indexes)
    }

    pub fn ticks_accounts_outdated(&self) -> bool {
        let ticks_addresses = self.get_ticks_addresses_around();

        ticks_addresses
            .iter()
            .any(|address| !self.ticks.contains_key(address))
    }

    fn extract_from_anchor_account(data: &[u8]) -> &[u8] {
        data.split_at(ANCHOR_DISCRIMINATOR_SIZE).1
    }

    fn find_closest_tick_indexes(
        &self,
        amount_limit: usize,
        direction: PriceDirection,
    ) -> Vec<i32> {
        let current: i32 = self.pool.current_tick_index;
        let tick_spacing: i32 = self.pool.tick_spacing.into();
        let tickmap = self.tickmap.bitmap;

        if current % tick_spacing != 0 {
            panic!("Invalid arguments: can't find initialized ticks")
        }
        let mut found: Vec<i32> = Vec::new();
        let current_index = current / tick_spacing + TICK_LIMIT;
        let (mut above, mut below, mut reached_limit) = (current_index + 1, current_index, false);

        while !reached_limit && found.len() < amount_limit {
            match direction {
                PriceDirection::UP => {
                    let value_above: u8 =
                        *tickmap.get((above / 8) as usize).unwrap() & (1 << (above % 8));
                    if value_above != 0 {
                        found.push(above);
                    }
                    reached_limit = above >= TICKMAP_SIZE;
                    above += 1;
                }
                PriceDirection::DOWN => {
                    let value_below: u8 =
                        *tickmap.get((below / 8) as usize).unwrap() & (1 << (below % 8));
                    if value_below != 0 {
                        found.insert(0, below);
                    }
                    reached_limit = below <= 0;
                    below -= 1;
                }
            }
        }

        found
            .iter()
            .map(|i: &i32| (i - TICK_LIMIT) * tick_spacing)
            .collect()
    }
}
