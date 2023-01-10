use anchor_lang::prelude::Pubkey;
use anchor_lang::{AnchorDeserialize, Key};
use invariant_types::structs::{Pool, Tickmap};
use jupiter_core::amm::{
    Amm, KeyedAccount, Quote, QuoteParams, SwapLegAndAccountMetas, SwapParams,
};
use solana_sdk::pubkey;
use std::collections::HashMap;

pub const ANCHOR_DISCRIMINATOR_SIZE: usize = 8;
pub const TICK_LIMIT: i32 = 44364;
pub const PROGRAM_ID: Pubkey = pubkey!("HyaB3W9q6XdA5xwpU4XnSZV94htfmbmqJXZcEbRaJutt");
pub const TICK_CROSSES_PER_IX: usize = 19;

#[derive(Clone, Copy, Debug, Default)]
pub struct JupiterInvariant {
    market_key: Pubkey,
    label: String,
    pool: Pool,
    tickmap: Tickmap,
    tick_addresses: Vec<Pubkey>,
}

enum PriceDirection {
    UP,
    DOWN,
}

impl JupiterInvariant {
    pub fn new_from_keyed_account(keyed_account: &KeyedAccount) -> Result<Self, ()> {
        // keyed_account.account.data.split_at()
        // let pool: Pool = Pool::try_from_slice(keyed_account.account.data.split_at(ANCHOR_DISCRIMINATOR_SIZE).1).unwrap();
        let pool = Self::deserialize::<Pool>(&keyed_account.account.data);

        Ok(Self {
            label: String::from("Invariant"),
            market_key: keyed_account.key,
            pool,
            ..Default::default()
        })
    }

    fn extract_from_anchor_account(data: &[u8]) -> &[u8] {
        data.split_at(ANCHOR_DISCRIMINATOR_SIZE).1
    }

    fn deserialize<T>(data: &[u8]) -> T
    where
        T: AnchorDeserialize,
    {
        T::try_from_slice(Self::extract_from_anchor_account(data)).unwrap()
    }

    fn find_closest_tick_indexes(
        self: Self,
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
        let current_index = current.checked_div(tick_spacing).unwrap();
        let mut above = current_index.checked_add(1).unwrap();
        let mut below = current_index;
        let mut reached = false;

        while !reached && found.len() < amount_limit {
            match direction {
                PriceDirection::UP => {
                    let value_above: u8 =
                        *tickmap.get((above / 8) as usize).unwrap() & (1 << (above % 8));
                    if value_above != 0 {
                        found.push(above);
                    }
                    reached = above >= 2 * TICK_LIMIT;
                    above += 1;
                }
                PriceDirection::DOWN => {
                    let value_below: u8 =
                        *tickmap.get((below / 8) as usize).unwrap() & (1 << (below % 8));
                    if value_below != 0 {
                        found.insert(0, above);
                    }
                    reached = below <= 0;
                    below -= 1;
                }
            }
        }
        found
    }

    fn tick_indexes_to_addresses(self: Self, indexes: &[i32]) -> Vec<Pubkey> {
        let pubkeys: Vec<Pubkey> = indexes
            .iter()
            .map(|i| {
                let (pubkey, _) = Pubkey::find_program_address(
                    &[b"tickv1", self.market_key.key().as_ref(), &i.to_le_bytes()],
                    &PROGRAM_ID,
                );
                pubkey
            })
            .collect();

        pubkeys

        // indexes.map();
        //
        // let (tick_address, _) = Pubkey::find_program_address(
        //     &[b"tickv1",
        //         self.market_key.key().as_ref(),
        //         indexes[0].to_le_bytes()],
        //     *PROGRAM_ID,
        // );
    }

    fn get_ticks_addresses_around(self: &Self) -> &[Pubkey] {
        self.find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::UP);

        let above_addresses = self
            .find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::UP)
            .as_slice();

        let below_addresses = self
            .find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::DOWN)
            .as_slice();

        let indexes = [above_addresses, below_addresses].concat().as_slice();
        self.tick_indexes_to_addresses(indexes).as_slice()
    }
}

impl Amm for JupiterInvariant {
    fn label(&self) -> String {
        todo!()
    }

    fn key(&self) -> Pubkey {
        todo!()
    }

    fn get_reserve_mints(&self) -> Vec<Pubkey> {
        todo!()
    }

    fn get_accounts_to_update(&self) -> Vec<Pubkey> {
        let ticks_addresses = self.get_ticks_addresses_around();
        let mut result = vec![self.market_key, self.pool.tickmap];
        result.extend(ticks_addresses);
        result
    }

    fn update(&mut self, accounts_map: &HashMap<Pubkey, Vec<u8>>) -> anyhow::Result<()> {
        let market_account_data: &[u8] = accounts_map.get(&self.market_key).unwrap();
        let tickmap_account_data: &[u8] = accounts_map.get(&self.pool.tickmap).unwrap();
        let pool = Self::deserialize::<Pool>(market_account_data);
        let tickmap = Self::deserialize::<Tickmap>(tickmap_account_data);

        self.pool = pool;
        self.tickmap = tickmap;

        Ok(())
    }
    fn quote(&self, quote_params: &QuoteParams) -> anyhow::Result<Quote> {
        todo!()
    }

    fn get_swap_leg_and_account_metas(
        &self,
        swap_params: &SwapParams,
    ) -> anyhow::Result<SwapLegAndAccountMetas> {
        todo!()
    }

    fn clone_amm(&self) -> Box<dyn Amm + Send + Sync> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::{AnchorDeserialize, AnchorSerialize};
    use decimal::*;
    use invariant_types::decimals::FixedPoint;
    use invariant_types::structs::{FeeTier, Pool};
    use solana_client::rpc_client::RpcClient;
    use solana_sdk::account::Account;
    use solana_sdk::pubkey;

    #[test]
    fn test_jupiter_invariant() {
        use anchor_lang::prelude::*;
        use solana_client::rpc_client::RpcClient;

        const USDC_USDT_MARKET: Pubkey = pubkey!("BRt1iVYDNoohkL1upEb8UfHE8yji6gEDAmuN9Y4yekyc");
        let rpc = RpcClient::new("https://tame-ancient-mountain.solana-mainnet.quiknode.pro/6a9a95bf7bbb108aea620e7ee4c1fd5e1b67cc62");
        let pool_account = rpc.get_account(&USDC_USDT_MARKET).unwrap();

        let market_account = KeyedAccount {
            key: USDC_USDT_MARKET,
            account: pool_account,
            params: None,
        };

        // create
        let mut jupiter_invariant =
            JupiterInvariant::new_from_keyed_account(&market_account).unwrap();

        // get accounts
        let accounts_to_update = jupiter_invariant.get_accounts_to_update();

        // get data from accounts
        let accounts_map = rpc
            .get_multiple_accounts(&accounts_to_update)
            .unwrap()
            .iter()
            .enumerate()
            .fold(HashMap::new(), |mut m, (index, account)| {
                if let Some(account) = account {
                    m.insert(accounts_to_update[index], account.data.clone());
                }
                m
            });

        jupiter_invariant.update(&accounts_map).unwrap();
    }

    // #[test]
    // fn test_deserialize_fee_tier() {
    //     const FEE_TIER_ADDRESS: Pubkey = pubkey!("EMuePmVq4YtAEoq1XZ9SVSSgUmAkWj25hLHSVghHA6GY");
    //     let rpc = RpcClient::new("https://tame-ancient-mountain.solana-mainnet.quiknode.pro/6a9a95bf7bbb108aea620e7ee4c1fd5e1b67cc62");
    //     let fee_tier_data: Account = rpc.get_account(&FEE_TIER_ADDRESS).unwrap();
    //     println!("{:?}", fee_tier_data);
    //
    //     let extracted_data = fee_tier_data.data.split_at(8).1;
    //     let fee_tier: FeeTier = FeeTier::try_from_slice(extracted_data).unwrap();
    //     println!("fee_tier = {:?}", fee_tier);
    // }
}
