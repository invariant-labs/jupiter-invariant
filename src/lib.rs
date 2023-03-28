pub mod tests;

use anyhow::{Error, Result};
use std::cell::RefCell;
use std::collections::HashMap;

use anchor_lang::prelude::{AccountMeta, Pubkey};
use anchor_lang::{AnchorDeserialize, Key};
use invariant_types::decimals::*;
use invariant_types::log::get_tick_at_sqrt_price;
use invariant_types::math::{
    compute_swap_step, cross_tick, get_closer_limit, get_max_sqrt_price, get_min_sqrt_price,
    is_enough_amount_to_push_price, SwapResult,
};
use invariant_types::structs::{
    Pool, Tick, Tickmap, TICKMAP_SIZE, TICK_CROSSES_PER_IX, TICK_LIMIT,
};
use invariant_types::{
    ANCHOR_DISCRIMINATOR_SIZE, ID, MAX_VIRTUAL_CROSS, SEED, STATE_SEED, TICK_SEED,
};
use jupiter::jupiter_override::{Swap, SwapLeg};
use jupiter_core::amm::{
    Amm, KeyedAccount, Quote, QuoteParams, SwapLegAndAccountMetas, SwapParams,
};
use solana_client::rpc_client::RpcClient;

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

#[derive(Clone)]
pub struct InvariantSwapParams<'a> {
    invariant_swap_result: &'a InvariantSwapResult,
    owner: Pubkey,
    source_mint: Pubkey,
    destination_mint: Pubkey,
    source_account: Pubkey,
    destination_account: Pubkey,
    referral_fee: Option<Pubkey>,
}

struct InvariantSimulationParams {
    in_amount: u64,
    x_to_y: bool,
    by_amount_in: bool,
    sqrt_price_limit: Price,
}

impl InvariantSwapAccounts {
    pub fn from_pubkeys(
        jupiter_invariant: &JupiterInvariant,
        invariant_swap_params: &InvariantSwapParams,
    ) -> anyhow::Result<(Self, bool), Error> {
        let InvariantSwapParams {
            invariant_swap_result,
            owner,
            source_mint,
            destination_mint,
            source_account,
            destination_account,
            referral_fee,
        } = invariant_swap_params;

        let (x_to_y, account_x, account_y) = match (
            jupiter_invariant.pool.token_x.eq(source_mint),
            jupiter_invariant.pool.token_y.eq(destination_mint),
            jupiter_invariant.pool.token_x.eq(destination_mint),
            jupiter_invariant.pool.token_y.eq(source_mint),
        ) {
            (true, true, _, _) => (true, *source_account, *destination_account),
            (_, _, true, true) => (false, *destination_account, *source_account),
            _ => return Err(anyhow::Error::msg("Invalid source or destination mint")),
        };
        // possibility update: add one tick in the opposite direction to swap direction
        let ticks_accounts =
            jupiter_invariant.tick_indexes_to_addresses(&invariant_swap_result.crossed_ticks);

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
        let ticks_metas: Vec<AccountMeta> = self
            .ticks_accounts
            .iter()
            .map(|tick_address| AccountMeta::new(*tick_address, false))
            .collect();
        account_metas.extend(ticks_metas);

        account_metas
    }

    fn get_program_authority(program_id: Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[SEED.as_bytes()], &program_id).0
    }

    fn get_state_address(program_id: Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[STATE_SEED.as_bytes()], &program_id).0
    }
}

enum PriceDirection {
    UP,
    DOWN,
}

#[derive(Clone, Default)]
struct InvariantSwapResult {
    in_amount: u64,
    out_amount: u64,
    fee_amount: u64,
    crossed_ticks: Vec<i32>,
    virtual_cross_counter: u16,
    global_insufficient_liquidity: bool,
}

impl InvariantSwapResult {
    pub fn is_not_enoght_liquidity(&self) -> bool {
        self.is_not_enoght_liquidity_referal(true)
    }

    pub fn is_exceeded_cu_referal(&self, is_referal: bool) -> bool {
        let crossed_amount = self.crossed_ticks.len();
        let mut max_cross = TICK_CROSSES_PER_IX;
        if is_referal {
            max_cross -= 1;
        }
        let is_excceded_by_account_size = crossed_amount > max_cross;
        let is_excceded_by_compute_units =
            crossed_amount == max_cross && self.virtual_cross_counter > MAX_VIRTUAL_CROSS;

        is_excceded_by_account_size || is_excceded_by_compute_units
    }

    pub fn is_not_enoght_liquidity_referal(&self, is_referal: bool) -> bool {
        self.is_exceeded_cu_referal(is_referal) || self.global_insufficient_liquidity
    }
}

impl JupiterInvariant {
    pub fn new_from_keyed_account(keyed_account: &KeyedAccount) -> Result<Self> {
        let pool = Self::deserialize::<Pool>(&keyed_account.account.data)?;

        Ok(Self {
            program_id: ID,
            label: String::from("Invariant"),
            market_key: keyed_account.key,
            pool,
            ..Default::default()
        })
    }

    fn extract_from_anchor_account(data: &[u8]) -> &[u8] {
        data.split_at(ANCHOR_DISCRIMINATOR_SIZE).1
    }

    fn deserialize<T>(data: &[u8]) -> anyhow::Result<T>
    where
        T: AnchorDeserialize,
    {
        T::try_from_slice(Self::extract_from_anchor_account(data))
            .map_err(|e| anyhow::anyhow!("Error deserializing account data: {:?}", e))
    }

    #[allow(dead_code)]
    fn fetch_accounts(
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

    fn tick_indexes_to_addresses(&self, indexes: &[i32]) -> Vec<Pubkey> {
        let pubkeys: Vec<Pubkey> = indexes
            .iter()
            .map(|i| self.tick_index_to_address(*i))
            .collect();
        pubkeys
    }

    fn tick_index_to_address(&self, i: i32) -> Pubkey {
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

    fn get_ticks_addresses_around(&self) -> Vec<Pubkey> {
        let above_indexes = self.find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::UP);
        let below_indexes =
            self.find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::DOWN);
        let all_indexes = [below_indexes, above_indexes].concat();

        self.tick_indexes_to_addresses(&all_indexes)
    }

    fn quote_to_invarinat_params(
        &self,
        quote_params: &QuoteParams,
    ) -> anyhow::Result<InvariantSimulationParams> {
        let QuoteParams {
            in_amount,
            input_mint,
            output_mint,
        } = *quote_params;

        let x_to_y = input_mint.eq(&self.pool.token_x);
        let sqrt_price_limit: Price = if x_to_y {
            get_min_sqrt_price(self.pool.tick_spacing)
        } else {
            get_max_sqrt_price(self.pool.tick_spacing)
        };

        let (expected_input_mint, expected_output_mint) = if x_to_y {
            (self.pool.token_x, self.pool.token_y)
        } else {
            (self.pool.token_y, self.pool.token_x)
        };
        if !(input_mint.eq(&expected_input_mint) && output_mint.eq(&expected_output_mint)) {
            return Err(anyhow::anyhow!("Invalid source or destination mint"));
        }
        Ok(InvariantSimulationParams {
            x_to_y,
            in_amount,
            by_amount_in: true,
            sqrt_price_limit,
        })
    }

    fn simulate_invariant_swap(
        &self,
        invariant_simulation_params: &InvariantSimulationParams,
    ) -> Result<InvariantSwapResult, &str> {
        let InvariantSimulationParams {
            in_amount,
            x_to_y,
            sqrt_price_limit,
            by_amount_in,
        } = *invariant_simulation_params;

        let (mut pool, ticks, tickmap) = (
            &mut self.pool.clone(),
            &self.ticks.clone(),
            &self.tickmap.clone(),
        );
        let (mut remaining_amount, mut total_amount_in, mut total_amount_out, mut total_fee_amount) = (
            TokenAmount::new(in_amount),
            TokenAmount::new(0),
            TokenAmount::new(0),
            TokenAmount::new(0),
        );
        let (mut crossed_ticks, mut virtual_cross_counter, mut global_insufficient_liquidity) =
            (Vec::new(), 0u16, false);

        while !remaining_amount.is_zero() {
            let (swap_limit, limiting_tick) = match get_closer_limit(
                sqrt_price_limit,
                x_to_y,
                pool.current_tick_index,
                pool.tick_spacing,
                tickmap,
            ) {
                Ok((swap_limit, limiting_tick)) => (swap_limit, limiting_tick),
                Err(_) => {
                    global_insufficient_liquidity = true;
                    break;
                }
            };

            let result: SwapResult = compute_swap_step(
                pool.sqrt_price,
                swap_limit,
                pool.liquidity,
                remaining_amount,
                by_amount_in,
                pool.fee,
            );

            remaining_amount -= result.amount_in + result.fee_amount;
            pool.sqrt_price = result.next_price_sqrt;
            total_amount_in += result.amount_in + result.fee_amount;
            total_amount_out += result.amount_out;
            total_fee_amount += result.fee_amount;

            if { pool.sqrt_price } == sqrt_price_limit && !remaining_amount.is_zero() {
                global_insufficient_liquidity = true;
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
                        cross_tick(&mut tick, pool)
                            .map_err(|_| "Internal Invariant Error: Cross tick")?;
                        crossed_ticks.push(tick.index);
                    } else if !remaining_amount.is_zero() {
                        total_amount_in += remaining_amount;
                        remaining_amount = TokenAmount(0);
                    }
                } else {
                    virtual_cross_counter += 1;
                }

                pool.current_tick_index = if x_to_y && is_enough_amount_to_cross {
                    tick_index.checked_sub(pool.tick_spacing as i32).unwrap()
                } else {
                    tick_index
                };
            } else {
                if pool
                    .current_tick_index
                    .checked_rem(pool.tick_spacing.into())
                    .unwrap()
                    != 0
                {
                    return Err("Internal Invariant Error: Invalid tick");
                }
                pool.current_tick_index =
                    get_tick_at_sqrt_price(result.next_price_sqrt, pool.tick_spacing);
                virtual_cross_counter += 1;
            }
        }
        Ok(InvariantSwapResult {
            in_amount: total_amount_in.0,
            out_amount: total_amount_out.0,
            fee_amount: total_fee_amount.0,
            crossed_ticks,
            virtual_cross_counter,
            global_insufficient_liquidity,
        })
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
        let market_account_data: &[u8] = accounts_map
            .get(&self.market_key)
            .ok_or_else(|| anyhow::anyhow!("Market account data not found"))?;
        let tickmap_account_data: &[u8] = accounts_map
            .get(&self.pool.tickmap)
            .ok_or_else(|| anyhow::anyhow!("Tickmap account data not found"))?;

        let pool = Self::deserialize::<Pool>(market_account_data)?;
        let tickmap = Self::deserialize::<Tickmap>(tickmap_account_data)?;

        let ticks = accounts_map
            .iter()
            .filter(|(key, _)| !self.market_key.eq(key) && !self.pool.tickmap.eq(key))
            .map(|(key, data)| {
                let tick = Self::deserialize::<Tick>(data)?;
                Ok((*key, tick))
            })
            .collect::<Result<HashMap<Pubkey, Tick>>>()?;

        self.ticks = ticks;
        self.pool = pool;
        self.tickmap = tickmap;

        Ok(())
    }

    fn quote(&self, quote_params: &QuoteParams) -> anyhow::Result<Quote> {
        let invariant_simulation_params = self.quote_to_invarinat_params(quote_params)?;
        let simulation_result = self.simulate_invariant_swap(&invariant_simulation_params);

        match simulation_result {
            Ok(result) => {
                let not_enough_liquidity = result.is_not_enoght_liquidity();
                let InvariantSwapResult {
                    in_amount,
                    out_amount,
                    fee_amount,
                    ..
                } = result;
                let quote = Quote {
                    in_amount,
                    out_amount,
                    fee_amount,
                    not_enough_liquidity,
                    ..Quote::default()
                };
                Ok(quote)
            }
            Err(_err) => Ok(Quote {
                not_enough_liquidity: true,
                ..Quote::default()
            }),
        }
    }

    fn get_swap_leg_and_account_metas(
        &self,
        swap_params: &SwapParams,
    ) -> anyhow::Result<SwapLegAndAccountMetas> {
        let SwapParams {
            in_amount,
            destination_mint,
            source_mint,
            user_destination_token_account,
            user_source_token_account,
            user_transfer_authority,
            ..
        } = *swap_params;

        let quote_params = QuoteParams {
            in_amount,
            input_mint: source_mint,
            output_mint: destination_mint,
        };
        let invarinat_simulation_params = self.quote_to_invarinat_params(&quote_params)?;
        let invariant_swap_result = self.simulate_invariant_swap(&invarinat_simulation_params);

        if let Err(_) = invariant_swap_result {
            return Err(anyhow::anyhow!("simulation error"));
        }
        let invariant_swap_result = invariant_swap_result.unwrap();
        if invariant_swap_result.is_not_enoght_liquidity() {
            return Err(anyhow::anyhow!("insufficient liquidity"));
        }

        let invariant_swap_params = InvariantSwapParams {
            invariant_swap_result: &invariant_swap_result,
            owner: user_transfer_authority,
            source_mint,
            destination_mint,
            source_account: user_source_token_account,
            destination_account: user_destination_token_account,
            referral_fee: None,
        };

        let (invariant_swap_accounts, x_to_y) =
            InvariantSwapAccounts::from_pubkeys(&self, &invariant_swap_params)?;
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
