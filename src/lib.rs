use std::cell::RefCell;
use std::collections::HashMap;

use anchor_lang::{AnchorDeserialize, Key};
use anchor_lang::prelude::{AccountMeta, Pubkey};
use anyhow::Error;
use invariant_types::{ANCHOR_DISCRIMINATOR_SIZE, SEED, STATE_SEED, TICK_SEED};
use invariant_types::decimals::*;
use invariant_types::log::get_tick_at_sqrt_price;
use invariant_types::math::{compute_swap_step, cross_tick, get_closer_limit, get_max_sqrt_price, get_min_sqrt_price, is_enough_amount_to_push_price, SwapResult};
use invariant_types::structs::{Pool, Tick, TICK_CROSSES_PER_IX, TICK_LIMIT, Tickmap, TICKMAP_SIZE};
use jupiter::jupiter_override::{Swap, SwapLeg};
use jupiter_core::amm::{
    Amm, KeyedAccount, Quote, QuoteParams, SwapLegAndAccountMetas, SwapParams,
};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey;

pub const PROGRAM_ID: Pubkey = pubkey!("HyaB3W9q6XdA5xwpU4XnSZV94htfmbmqJXZcEbRaJutt");

#[derive(Clone, Default)]
pub struct JupiterInvariant {
    program_id: Pubkey,
    market_key: Pubkey,
    label: String,
    pool: Pool,
    tickmap: Tickmap,
    ticks: HashMap<Pubkey, Tick>,
}

#[derive(Clone, Default, Debug)]
pub struct InvariantSwapAccounts {
    state: Pubkey,
    pool: Pubkey,
    tickmap: Pubkey,
    account_x: Pubkey,
    account_y: Pubkey,
    reserve_x: Pubkey,
    reserve_y: Pubkey,
    owner: Pubkey,
    program_authority: Pubkey,
    token_program: Pubkey,
    ticks_accounts: Vec<Pubkey>,
    referral_fee: Option<Pubkey>,
}

#[derive(Clone, Default)]
pub struct InvariantSwapParams {
    owner: Pubkey,
    source_mint: Pubkey,
    destination_mint: Pubkey,
    source_account: Pubkey,
    destination_account: Pubkey,
    referral_fee: Option<Pubkey>,
}

impl InvariantSwapAccounts {
    pub fn from_pubkeys(jupiter_invariant: &JupiterInvariant, invariant_swap_params: &InvariantSwapParams) -> Result<(Self, bool), Error> {
        let InvariantSwapParams { owner, source_mint, destination_mint, source_account, destination_account, referral_fee } = invariant_swap_params;

        let (x_to_y, account_x, account_y) = match (
            jupiter_invariant.pool.token_x.eq(source_mint),
            jupiter_invariant.pool.token_y.eq(destination_mint),
            jupiter_invariant.pool.token_x.eq(destination_mint),
            jupiter_invariant.pool.token_y.eq(source_mint)) {
            (true, true, _, _) => (true, *source_account, *destination_account),
            (_, _, true, true) => (false, *destination_account, *source_account),
            _ => return Err(Error::msg("Invalid source or destination mint")),
        };
        let max_ticks_account_size = if referral_fee.is_none() {
            TICK_CROSSES_PER_IX
        } else {
            TICK_CROSSES_PER_IX - 1
        };
        let (ticks_above_amount, ticks_below_amount) = if x_to_y {
            (1, max_ticks_account_size - 1)
        } else {
            (max_ticks_account_size - 1, 1)
        };

        let tick_indexes_above = jupiter_invariant.find_closest_tick_indexes(ticks_above_amount, PriceDirection::UP);
        let tick_indexes_below = jupiter_invariant.find_closest_tick_indexes(ticks_below_amount, PriceDirection::DOWN);
        let all_tick_indexes = [tick_indexes_below, tick_indexes_above].concat();
        let ticks_accounts = jupiter_invariant.tick_indexes_to_addresses(&all_tick_indexes);

        let invariant_swap_accounts = Self {
            state: Self::get_state_address(jupiter_invariant.program_id),
            pool: jupiter_invariant.market_key,
            tickmap: jupiter_invariant.pool.tickmap,
            account_x,
            account_y,
            reserve_x: jupiter_invariant.pool.token_x_reserve,
            reserve_y: jupiter_invariant.pool.token_y_reserve,
            owner: *owner,
            program_authority: Self::get_program_authority(jupiter_invariant.program_id),
            token_program: spl_token::id(),
            ticks_accounts,
            referral_fee: *referral_fee,
        };

        Ok((invariant_swap_accounts, x_to_y))
    }

    pub fn to_account_metas(&self) -> Vec<AccountMeta> {
        let mut account_metas: Vec<AccountMeta> = vec![
            AccountMeta::new_readonly(self.state, false),
            AccountMeta::new(self.pool, false),
            AccountMeta::new(self.tickmap, false),
            AccountMeta::new(self.account_x, false),
            AccountMeta::new(self.account_y, false),
            AccountMeta::new(self.reserve_x, false),
            AccountMeta::new(self.reserve_y, false),
            AccountMeta::new(self.owner, true),
            AccountMeta::new_readonly(self.program_authority, false),
            AccountMeta::new_readonly(self.token_program, false),
        ];
        if let Some(referral_fee) = self.referral_fee {
            account_metas.push(AccountMeta::new(referral_fee, false));
        }
        let ticks_metas: Vec<AccountMeta> = self.ticks_accounts
            .iter()
            .map(|tick_address| AccountMeta::new(*tick_address, false))
            .collect();
        account_metas.extend(ticks_metas);

        account_metas
    }

    fn get_program_authority(program_id: Pubkey) -> Pubkey {
        Pubkey::find_program_address(
            &[SEED.as_bytes()],
            &program_id,
        ).0
    }

    fn get_state_address(program_id: Pubkey) -> Pubkey {
        Pubkey::find_program_address(
            &[STATE_SEED.as_bytes()],
            &program_id,
        ).0
    }
}

enum PriceDirection {
    UP,
    DOWN,
}

struct InvariantSwapResult {
    in_amount: u64,
    out_amount: u64,
    fee_amount: u64,
    tick_required: u16,
    insufficient_liquidity: bool,
}

impl JupiterInvariant {
    pub fn new_from_keyed_account(keyed_account: &KeyedAccount) -> Result<Self, ()> {
        let pool = Self::deserialize::<Pool>(&keyed_account.account.data);

        Ok(Self {
            program_id: PROGRAM_ID,
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

    #[allow(dead_code)]
    fn fetch_accounts(rpc: &RpcClient, accounts_to_update: Vec<Pubkey>) -> HashMap<Pubkey, Vec<u8>> {
        rpc
            .get_multiple_accounts(&accounts_to_update)
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

        found.iter().map(|i: &i32| {
            (i - TICK_LIMIT) * tick_spacing
        }).collect()
    }

    fn tick_indexes_to_addresses(&self, indexes: &[i32]) -> Vec<Pubkey> {
        let pubkeys: Vec<Pubkey> = indexes
            .iter()
            .map(|i| {
                self.tick_index_to_address(*i)
            })
            .collect();
        pubkeys
    }

    fn tick_index_to_address(&self, i: i32) -> Pubkey {
        let (pubkey, _) = Pubkey::find_program_address(
            &[TICK_SEED.as_bytes(), self.market_key.key().as_ref(), &i.to_le_bytes()],
            &self.program_id,
        );
        pubkey
    }

    fn get_ticks_addresses_around(&self) -> Vec<Pubkey> {
        let above_indexes = self
            .find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::UP);
        let below_indexes = self
            .find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::DOWN);
        let all_indexes = [below_indexes, above_indexes].concat();

        self.tick_indexes_to_addresses(&all_indexes)
    }
}

impl Amm for JupiterInvariant {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn key(&self) -> Pubkey {
        self.market_key
    }

    fn get_reserve_mints(&self) -> Vec<Pubkey> {
        vec![self.pool.token_x, self.pool.token_y]
    }

    fn get_accounts_to_update(&self) -> Vec<Pubkey> {
        let mut ticks_addresses = self.get_ticks_addresses_around();
        ticks_addresses.extend([self.market_key, self.pool.tickmap]);
        ticks_addresses
    }

    fn update(&mut self, accounts_map: &HashMap<Pubkey, Vec<u8>>) -> anyhow::Result<()> {
        let market_account_data: &[u8] = accounts_map.get(&self.market_key).unwrap();
        let tickmap_account_data: &[u8] = accounts_map.get(&self.pool.tickmap).unwrap();
        let pool = Self::deserialize::<Pool>(market_account_data);
        let tickmap = Self::deserialize::<Tickmap>(tickmap_account_data);

        let ticks = accounts_map.iter()
            .filter(|(key, _)| !self.market_key.eq(key) && !self.pool.tickmap.eq(key))
            .map(|(key, data)| {
                let tick = Self::deserialize::<Tick>(data);
                (*key, tick)
            })
            .collect::<HashMap<Pubkey, Tick>>();

        self.ticks = ticks;
        self.pool = pool;
        self.tickmap = tickmap;

        Ok(())
    }

    fn quote(&self, quote_params: &QuoteParams) -> anyhow::Result<Quote> {
        let QuoteParams {
            in_amount,
            input_mint,
            output_mint,
        } = *quote_params;
        let x_to_y = input_mint.eq(&self.pool.token_x);
        let by_amount_in = true; // always by amount in
        let sqrt_price_limit: Price = if x_to_y { get_min_sqrt_price(self.pool.tick_spacing) } else { get_max_sqrt_price(self.pool.tick_spacing) };

        let (expected_input_mint, expected_output_mint) = if x_to_y {
            (self.pool.token_x, self.pool.token_y)
        } else {
            (self.pool.token_y, self.pool.token_x)
        };
        if !(input_mint.eq(&expected_input_mint) && output_mint.eq(&expected_output_mint)) {
            panic!("Invalid source or destination mint");
        }

        let calculate_amount_out = || -> Result<InvariantSwapResult, ()> {
            let (mut pool, ticks, tickmap) =
                (&mut self.pool.clone(), &self.ticks.clone(), &self.tickmap.clone());
            let (mut remaining_amount, mut total_amount_in, mut total_amount_out, mut total_fee_amount) =
                (TokenAmount::new(in_amount), TokenAmount::new(0), TokenAmount::new(0), TokenAmount::new(0));
            let (mut tick_required, mut insufficient_liquidity) = (0, false);

            while !remaining_amount.is_zero() {
                let (swap_limit, limiting_tick) =
                    get_closer_limit(sqrt_price_limit, x_to_y, pool.current_tick_index, pool.tick_spacing, &tickmap).unwrap();
                let result: SwapResult = compute_swap_step(pool.sqrt_price, swap_limit, pool.liquidity, remaining_amount, by_amount_in, pool.fee);

                remaining_amount -= result.amount_in + result.fee_amount;
                pool.sqrt_price = result.next_price_sqrt;
                total_amount_in += result.amount_in + result.fee_amount;
                total_amount_out += result.amount_out;
                total_fee_amount += result.fee_amount;

                // Fail if price would go over swap limit
                if { pool.sqrt_price } == sqrt_price_limit && !remaining_amount.is_zero() {
                    insufficient_liquidity = true;
                    break;
                }

                // crossing tick
                if result.next_price_sqrt == swap_limit && limiting_tick.is_some() {
                    let (tick_index, initialized) = limiting_tick.unwrap();
                    let is_enough_amount_to_cross = is_enough_amount_to_push_price(
                        remaining_amount,
                        result.next_price_sqrt,
                        pool.liquidity,
                        pool.fee,
                        by_amount_in,
                        x_to_y,
                    );

                    if initialized {
                        let tick_address = self.tick_index_to_address(tick_index);
                        let tick = RefCell::new(*ticks.get(&tick_address).unwrap());
                        let mut tick = tick.borrow_mut();

                        // crossing tick
                        if !x_to_y || is_enough_amount_to_cross {
                            cross_tick(&mut tick, &mut pool).unwrap();
                        } else if !remaining_amount.is_zero() {
                            total_amount_in += remaining_amount;
                            remaining_amount = TokenAmount(0);
                        }
                        tick_required += 1;
                    }
                    // set tick to limit (below if price is going down, because current tick should always be below price)
                    pool.current_tick_index = if x_to_y && is_enough_amount_to_cross {
                        tick_index.checked_sub(pool.tick_spacing as i32).unwrap()
                    } else {
                        tick_index
                    };
                } else {
                    if pool.current_tick_index
                        .checked_rem(pool.tick_spacing.into())
                        .unwrap()
                        != 0 {
                        panic!("tick not divisible by spacing");
                    }
                    pool.current_tick_index =
                        get_tick_at_sqrt_price(result.next_price_sqrt, pool.tick_spacing);
                }
            }
            Ok(InvariantSwapResult {
                in_amount: total_amount_in.0,
                out_amount: total_amount_out.0,
                fee_amount: total_fee_amount.0,
                tick_required,
                insufficient_liquidity,
            })
        };

        let result = calculate_amount_out();
        match result {
            Ok(result) => {
                let InvariantSwapResult {
                    in_amount,
                    out_amount,
                    fee_amount,
                    tick_required,
                    insufficient_liquidity,
                } = result;
                let not_enough_liquidity = if insufficient_liquidity {
                    true
                } else {
                    tick_required >= TICK_CROSSES_PER_IX as u16
                };
                Ok(Quote {
                    in_amount,
                    out_amount,
                    fee_amount,
                    not_enough_liquidity,
                    ..Quote::default()
                })
            }
            Err(_err) => {
                Ok(Quote {
                    not_enough_liquidity: true,
                    ..Quote::default()
                })
            }
        }
    }

    fn get_swap_leg_and_account_metas(
        &self,
        swap_params: &SwapParams,
    ) -> anyhow::Result<SwapLegAndAccountMetas> {
        let SwapParams {
            destination_mint,
            source_mint,
            user_destination_token_account,
            user_source_token_account,
            user_transfer_authority,
            ..
        } = swap_params;

        let invariant_swap_params = InvariantSwapParams {
            owner: *user_transfer_authority,
            source_mint: *source_mint,
            destination_mint: *destination_mint,
            source_account: *user_source_token_account,
            destination_account: *user_destination_token_account,
            referral_fee: None,
        };

        let (invariant_swap_accounts, x_to_y) = InvariantSwapAccounts::from_pubkeys(&self, &invariant_swap_params)?;
        let account_metas = invariant_swap_accounts.to_account_metas();

        Ok(SwapLegAndAccountMetas {
            swap_leg: SwapLeg::Swap {
                swap: Swap::Invariant { x_to_y },
            },
            account_metas,
        })
    }

    fn clone_amm(&self) -> Box<dyn Amm + Send + Sync> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use anchor_lang::prelude::Pubkey;
    use solana_client::rpc_client::RpcClient;
    use solana_sdk::pubkey;

    use super::*;

    #[test]
    fn test_jupiter_invariant() {
        use anchor_lang::prelude::*;
        use solana_client::rpc_client::RpcClient;

        const USDC_USDT_MARKET: Pubkey = pubkey!("BRt1iVYDNoohkL1upEb8UfHE8yji6gEDAmuN9Y4yekyc");
        const USDC: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        const USDT: Pubkey = pubkey!("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB");
        let rpc = RpcClient::new("https://api.mainnet-beta.solana.com");
        let pool_account = rpc.get_account(&USDC_USDT_MARKET).unwrap();

        let market_account = KeyedAccount {
            key: USDC_USDT_MARKET,
            account: pool_account,
            params: None,
        };

        // create JupiterInvariant
        let mut jupiter_invariant =
            JupiterInvariant::new_from_keyed_account(&market_account).unwrap();

        // update market data
        let accounts_to_update = jupiter_invariant.get_accounts_to_update();
        let accounts_map = JupiterInvariant::fetch_accounts(&rpc, accounts_to_update);
        jupiter_invariant.update(&accounts_map).unwrap();

        // update once again due to fetch accounts on a non-initialized tickmap.
        let accounts_to_update = jupiter_invariant.get_accounts_to_update();
        let accounts_map = JupiterInvariant::fetch_accounts(&rpc, accounts_to_update);
        jupiter_invariant.update(&accounts_map).unwrap();

        let quote = QuoteParams {
            in_amount: 1 * 10u64.pow(6),
            input_mint: USDC,
            output_mint: USDT,
        };
        let result = jupiter_invariant.quote(&quote).unwrap();

        println!("insufficient liquidity: {:?}", result.not_enough_liquidity);
        println!("input amount: {:.6} USDC", result.in_amount as f64 / 10u64.pow(6) as f64);
        println!("output amount: {:.6} USDT", result.out_amount as f64 / 10u64.pow(6) as f64);
        println!("fee amount: {:.6} USDC", result.fee_amount as f64 / 10u64.pow(6) as f64);

        let _swap_leg_and_account_metas = jupiter_invariant.get_swap_leg_and_account_metas(&SwapParams {
            source_mint: quote.input_mint,
            destination_mint: quote.output_mint,
            user_destination_token_account: Pubkey::new_unique(),
            user_source_token_account: Pubkey::new_unique(),
            user_transfer_authority: Pubkey::new_unique(),
            open_order_address: None,
            quote_mint_to_referrer: None,
            in_amount: quote.in_amount,
        }).unwrap();
    }

    #[test]
    fn test_fetch_all_pool() {
        let rpc = RpcClient::new("https://api.mainnet-beta.solana.com");
        let pool_addresses = vec![
            "966SEWSx1Dyx9hYMJxiUt3E2uer2HfdCgEmfBpkk5ovL",
            "6MC1F8kUvUMRo853ZFwhQVd5mSoLxmxN1Q2s724U3Gkd",
            "9bXJSJ3tGjkk8QVSykcGE6KnzdKznfjWP7YXicSRFTe8",
            "JCquHzZYEnM2c4Egx31wN9BwA6ogY9Lg88Uyhfg4CTwe",
            "9dBgnqQWLauQGZPr1R5196qgSqXdU3umVY3ZvnrExa68",
            "9wiBzTBVo1Ls5dHbzSNbiAqJAuonA44tPGRkBz6mx5Th",
            "AWsByD4jdTBVDJrtCzjDCE7Qcs2LNcvWCVa6BV27nMMQ",
            "4XVJ5mZB2h5fmcMJYasc6QReQQTNhJCQpevwA666diwN",
            "EkRnwUnggNm93G9NmPS8cGMwArffiRnmP8R6VBxbhcZX",
            "FwiuNR91xfiUvWiBu4gieK4SFmh9qjMhYS9ebyYJ8PGj",
            "BW3mgKMZNv9PzdwiCXLW4rzzXcNAZrs9wQc9m9Xca6rg",
            "A5U2LbZhwc6GFZtYft6MtbGHFhYyZ1pzCRQkz7D2b2YS",
            "2bRJLkxFbhL2aT3mjmzZegp6jQZa3UcDkuH37ePqMSuX",
            "2WLM9BDFMpMtzRrfWtPZRrUtquwdBRnBiC8ahqS6upCS",
            "BGz6pBLvc4nuGNsbXYoAE7z466kSjZaoXKJMnEkZWkyD",
            "EGQ86TNvchBYmNujCdVX4LVKpqhH2MGnCYxSuREsarMn",
            "8W7q7xurgBaNqLzc5iNTm46An1fWPCRzhPTmJyy8QYuA",
            "Gcdv7xvxQNzrX3Ks4GF1P1TyGzJZHpe3CPr8zRJ8Q1GT",
            "GmBMZ8BNeNR6mRFqu2ZaFvpxvGPXmc9Aa4a1GWFaxWMv",
            "DzCbrMZXNG3XpXuufhBp233LJXvV8Xq1C3q4ksrtbCPP",
            "29XTcKRGCtZARBSG8PZ2C41PKSeJXxFnYr7ivBMzRtse",
            "FmuR66XKWLuXW1ZZkWwy1DThxdTtUMesBoahXu3MjNEs",
            "4FkGNJMvKFk9PFwn8TBtk1ShUKege6D5Au87ezwLWiqk",
            "AXSXYioiHGFvQ1XF4zCvXjnVAcMY68WQ4RH5r5nAdah5",
            "9fY5jLin2yYMK9Eee5diWzJRZqLaYgEUupUNsKVYAMhi",
            "5yKHz86cvHocaQnxv4mPJjftA4w7BbJzQQr5rG9miyZ3",
            "2RKBg6QQi6MF7kjC51uFsFhDE1ydh6cNK1wY5iA8Rpdt",
            "9PDif8wTTXbiJry8hviXtiiyCyyvLpqxWEJjPVSgEb73",
            "9HqxfFV3DtvcALPDt3iici6naNYVa6Mzp9KV1sAwb4rX",
            "E72s83uakUDhkx4ZHt9YAj8qJeF2wKgHr95C1T6gYJEV",
            "2U9nDD4XNrwDU2NXXNQDDckLpBpfHdXoWoahEnu75TjB",
            "HYCzZYupXUyamfTQWNkULW1EFcNPCTA5urnEXCFXkLRh",
            "BRt1iVYDNoohkL1upEb8UfHE8yji6gEDAmuN9Y4yekyc",
            "6rvpVhL9fxm2WLMefNRaLwv6aNdivZadMi56teWfSkuU",
            "HbMbeaDH8xtB1a8WpwjNqcXBBGraKJjJ2xFkXEdAy1rY",
            "FejEVPJBH5TEbgcpupNuRyiMRoeaCQwPvtVU6w3i2xRc",
            "F2RieMDdnWyydUa8prEUz9Xv9k6Cny3kqF79ufTiiTTN",
            "C3tJvXV9zrHXDSu1QXYS1WEbo2rmpZpVbDzdGALkXo4H",
            "2dYDXu7uU5rTzrEYRYxGwBvqB6XNMHGJLyBsczYdxCX6",
            "9PxA7PANvfiTohha8PApJ2VHCFPUHdWYnyiGdv3R6sn8",
            "AvNeVrKZy1FaEG9suboRXNPgmnMwomiU5EvkF6jGxGrX",
            "EWZW9aJmY2LX6ZyV5RU8waHWKF1aGaxzbuBuRQp6G4j",
            "2SgUGxYDczrB6wUzXHPJH65pNhWkEzNMEx3km4xTYUTC",
            "C7BLPzc1vzLL3Tm5udXELnHDRXeXkdd5f1oMKP8rwUNv",
            "FeKSMTD9Z22kdnMLeNWAfaTXydbhVixUPcfZDW3QS1X4",
            "5dX3tkVDmbHBWMCQMerAHTmd9wsRvmtKLoQt6qv9fHy7",
            "7drJL7kEdJfgNzeaEYKDqTkfmiG2ain3nEPtGHDZc6i2",
            "3vRuk97EaKACp1Z337PvVWNdab57hbDwefdi1zoUg46D",
            "2keDsfcMY6hLvfrmzDAfnMSudMqYczPVcb8MSkK59P9r",
            "B2Mq1fpJ2bZYxxtF4yz6nNvLJLaYzM3zQsHcs2oDqk3z",
        ];
        let pubkeys: Vec<Pubkey> = pool_addresses.iter().map(|p| { return Pubkey::from_str(*p).unwrap(); }).collect::<Vec<Pubkey>>();

        rpc.get_multiple_accounts(&pubkeys).unwrap().iter().enumerate().for_each(|(index, market_account)| {
            let key: Pubkey = pubkeys.get(index).unwrap().to_owned();
            let account = market_account.to_owned().unwrap();
            let mut jupiter_invariant =
                JupiterInvariant::new_from_keyed_account(&KeyedAccount {
                    key,
                    account,
                    params: None,
                }).unwrap();
            let accounts_to_update = jupiter_invariant.get_accounts_to_update();
            let accounts_map = JupiterInvariant::fetch_accounts(&rpc, accounts_to_update);
            jupiter_invariant.update(&accounts_map).unwrap();
            let accounts_to_update = jupiter_invariant.get_accounts_to_update();
            let accounts_map = JupiterInvariant::fetch_accounts(&rpc, accounts_to_update);
            jupiter_invariant.update(&accounts_map).unwrap();

            let (user_transfer_authority, user_token_x_account, user_token_y_account) =
                (Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique());

            for i in 0..2 {
                let x_to_y = i % 2 == 0;

                let (source_mint, user_source_token_account, destination_mint, user_destination_token_account) = if x_to_y {
                    (jupiter_invariant.pool.token_x, user_token_x_account, jupiter_invariant.pool.token_y, user_token_y_account)
                } else {
                    (jupiter_invariant.pool.token_y, user_token_y_account, jupiter_invariant.pool.token_x, user_token_x_account)
                };

                let swap_params = SwapParams {
                    source_mint,
                    destination_mint,
                    user_source_token_account,
                    user_destination_token_account,
                    user_transfer_authority,
                    open_order_address: None,
                    quote_mint_to_referrer: None,
                    in_amount: 1, // amount doesn't matter, as the space for tickets is entirely filled.
                };
                let _swap_leg_and_account_metas = jupiter_invariant.get_swap_leg_and_account_metas(&swap_params).unwrap();
            }
        });
    }
}
