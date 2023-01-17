use std::borrow::Borrow;
use std::cell::RefCell;
use anchor_lang::prelude::{AccountMeta, Pubkey};
use anchor_lang::{AnchorDeserialize, Key};
use invariant_types::structs::{MAX_TICK, Pool, Tick, Tickmap};
use jupiter_core::amm::{
    Amm, KeyedAccount, Quote, QuoteParams, SwapLegAndAccountMetas, SwapParams,
};
use solana_sdk::pubkey;
use std::collections::HashMap;
use anyhow::Error;
use invariant_types::decimals::*;
use invariant_types::errors::InvariantErrorCode;
use invariant_types::log::get_tick_at_sqrt_price;
use invariant_types::math::{calculate_price_sqrt, compute_swap_step, cross_tick, get_closer_limit, is_enough_amount_to_push_price, SwapResult};
use invariant_types::{SEED, STATE_SEED, TICK_SEED};
use jupiter::jupiter_override::{Swap, SwapLeg};
use solana_client::rpc_client::RpcClient;

pub const ANCHOR_DISCRIMINATOR_SIZE: usize = 8;
pub const TICK_LIMIT: i32 = 44364;
pub const TICKMAP_SIZE: i32 = 2 * TICK_LIMIT - 1;
pub const PROGRAM_ID: Pubkey = pubkey!("HyaB3W9q6XdA5xwpU4XnSZV94htfmbmqJXZcEbRaJutt");
// pub const PROGRAM_ID: Pubkey = pubkey!("9aiirQKPZ2peE9QrXYmsbTtR7wSDJi2HkQdHuaMpTpei"); // devnet
pub const TICK_CROSSES_PER_IX: usize = 19;

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
            (true, true, _, _) => (true, *source_account, *destination_account, ),
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
        if self.referral_fee.is_some() {
            account_metas.push(AccountMeta::new(self.referral_fee.unwrap(), false));
        };
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
        let current_index = current.checked_div(tick_spacing).unwrap().checked_add(TICK_LIMIT).unwrap();
        let mut above = current_index.checked_add(1).unwrap();
        let mut below = current_index;
        let mut reached_limit = false;

        // println!("len of tickmap = {:?}", tickmap.len());
        while !reached_limit && found.len() < amount_limit {
            match direction {
                PriceDirection::UP => {
                    // if above / 8 > 0 {
                    // println!("index = {:?}", above / 8);
                    // println!("above = {:?}", above);
                    // println!("2 * TICK_LIMIT = {:?}", 2 * TICK_LIMIT);
                    // }
                    let value_above: u8 =
                        *tickmap.get((above / 8) as usize).unwrap() & (1 << (above % 8));
                    if value_above != 0 {
                        found.push(above);
                    }
                    reached_limit = above >= TICKMAP_SIZE;
                    above += 1;
                }
                PriceDirection::DOWN => {
                    // if below / 8 < 2 {
                    //     println!("index = {:?}", below / 8);
                    //     println!("above = {:?}", below);
                    // }
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

        // translate index in tickmap to price tick index
        found.iter().map(|i: &i32| {
            let i = i.checked_sub(TICK_LIMIT).unwrap();
            i.checked_mul(tick_spacing).unwrap()
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
        // self.tickmap.bitmap.iter().for_each(|b| {
        //     if *b != 0 {
        //         println!("find non-zero byte");
        //     }
        // });
        let above_indexes = self
            .find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::UP);
        // println!("above: {:?}", above_addresses);

        let below_indexes = self
            .find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::DOWN);
        // println!("below: {:?}", below_addresses);

        let all_indexes = [below_indexes, above_indexes].concat();
        // println!("all ticks indexes = {:?}", all_indexes);
        self.tick_indexes_to_addresses(&all_indexes)
    }

    fn get_max_sqrt_price(tick_spacing: u16) -> Price {
        let limit_by_space = TICK_LIMIT.checked_sub(1).unwrap().checked_mul(tick_spacing.into()).unwrap();
        let max_tick = limit_by_space.min(MAX_TICK);
        calculate_price_sqrt(max_tick)
    }

    fn get_min_sqrt_price(tick_spacing: u16) -> Price {
        let limit_by_space = (-TICK_LIMIT).checked_add(1).unwrap().checked_mul(tick_spacing.into()).unwrap();
        let min_tick = limit_by_space.max(-MAX_TICK);
        calculate_price_sqrt(min_tick)
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

        let ticks = accounts_map.into_iter()
            .filter(|(key, _)| !self.market_key.eq(key) && !self.pool.tickmap.eq(key))
            .collect::<HashMap<&Pubkey, &Vec<u8>>>().into_iter().map(|(key, data)| {
            let tick = Self::deserialize::<Tick>(data);
            (*key, tick)
        }).collect::<HashMap<Pubkey, Tick>>();

        self.ticks = ticks;
        self.pool = pool;
        self.tickmap = tickmap;

        Ok(())
    }

    fn quote(&self, quote_params: &QuoteParams) -> anyhow::Result<Quote> {
        // always by token_in
        let QuoteParams {
            in_amount,
            input_mint,
            output_mint,
        } = *quote_params;
        let x_to_y = input_mint.eq(&self.pool.token_x);
        let by_amount_in = true;
        let sqrt_price_limit: Price = (if x_to_y { Self::get_min_sqrt_price(self.pool.tick_spacing) } else { Self::get_max_sqrt_price(self.pool.tick_spacing) });

        let (expected_input_mint, expected_output_mint) = if x_to_y {
            (self.pool.token_x, self.pool.token_y)
        } else {
            (self.pool.token_y, self.pool.token_x)
        };
        if !(input_mint.eq(&expected_input_mint) && output_mint.eq(&expected_output_mint)) {
            panic!("Invalid quote params: token mints");
        }

        let calculate_amount_out = || -> Result<InvariantSwapResult, ()> {
            let mut pool: RefCell<Pool> = RefCell::new(self.pool.clone());
            let mut ticks: RefCell<HashMap<Pubkey, Tick>> = RefCell::new(self.ticks.clone());
            let mut tickmap: RefCell<Tickmap> = RefCell::new(self.tickmap.clone());
            let mut pool = pool.borrow_mut();
            let mut tickmap = tickmap.borrow_mut();
            let mut ticks = ticks.borrow_mut();
            let mut remaining_amount = TokenAmount::new(in_amount);
            let mut total_amount_in: TokenAmount = TokenAmount::new(0);
            let mut total_amount_out: TokenAmount = TokenAmount::new(0);
            let mut total_fee_amount: TokenAmount = TokenAmount::new(0);
            let mut tick_required = 0;
            let mut insufficient_liquidity = false;
            while !remaining_amount.is_zero() {
                let (swap_limit, limiting_tick) =
                    get_closer_limit(sqrt_price_limit, x_to_y, pool.current_tick_index, pool.tick_spacing, &tickmap).unwrap();
                let result: SwapResult = compute_swap_step(pool.sqrt_price, swap_limit, pool.liquidity, remaining_amount, by_amount_in, pool.fee);

                remaining_amount -= result.amount_in + result.fee_amount;
                println!("limiting_tick: {:?}", limiting_tick);
                println!("previous price : {:?}", { pool.sqrt_price });
                println!("next price: {:?}", { result.next_price_sqrt });
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
                        let tick: Tick = (*self.ticks.get(&tick_address).unwrap()).clone();
                        let mut tick = RefCell::new(tick);
                        let mut tick = tick.borrow_mut();

                        // crossing tick
                        if !x_to_y || is_enough_amount_to_cross {
                            cross_tick(&mut tick, &mut pool).unwrap();
                        } else if !remaining_amount.is_zero() {
                            total_amount_in += remaining_amount;
                            remaining_amount = TokenAmount(0);
                        }
                        ticks.insert(tick_address, tick.clone());
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
                // std::thread::sleep_ms(50);
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
    use std::ops::Rem;
    use std::str::FromStr;

    use super::*;
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::{AnchorDeserialize, AnchorSerialize};
    use decimal::*;
    use invariant_types::decimals::FixedPoint;
    use invariant_types::structs::{FeeTier, Pool};
    use solana_client::client_error::reqwest::blocking::get;
    use solana_client::rpc_client::RpcClient;
    use solana_sdk::account::Account;
    use solana_sdk::pubkey;

    #[test]
    fn test_jupiter_invariant() {
        use anchor_lang::prelude::*;
        use solana_client::rpc_client::RpcClient;

        const USDC_USDT_MARKET: Pubkey = pubkey!("BRt1iVYDNoohkL1upEb8UfHE8yji6gEDAmuN9Y4yekyc");
        const USDC: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        const USDT: Pubkey = pubkey!("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB");
        let rpc = RpcClient::new("https://tame-ancient-mountain.solana-mainnet.quiknode.pro/6a9a95bf7bbb108aea620e7ee4c1fd5e1b67cc62");
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
            in_amount: 15_000 * 10u64.pow(6),
            input_mint: USDT,
            output_mint: USDC,
        };
        let result = jupiter_invariant.quote(&quote).unwrap();
        println!("{:?}", result);
    }

    #[test]
    fn test_fetch_all_pool() {
        let rpc = RpcClient::new("https://divine-multi-research.solana-devnet.discover.quiknode.pro/51c4bf9b6daf33ca9abf53799a89a17088f68a77");
        let pool_addresses = vec![
            "8krGUkrubzsymUFmy5ncJHiE5RwyDEWazvwfahii7UW1",
            "51DNyWAJ7pa67DzkbHfipbZYW5hUSSRYsec5iHndtZ2W",
            "BZSp8EvV8XTfNrsyBXrF2xGB3L7SbHSwhmJ7RD74zmAH",
            "3a3EdoS5MvDcw9K8GdAb2FD7A9cUK6TuAqboJX23DQNM",
            "71QzPrqM26vMDJdtanPqyNyQ7MzqVw3FqzUWTpdNxta2",
            "4NkYxZtSpUzQc2oxktQoRYUSYC4awrtspuAGLq7Sx8A1",
            "Fb8rVemMjXE9r7FNK7DTmUdDhQDa3jFufkqrH2bsDYWX",
            "FKh586sYqro7RsTYsr7Wwc6b95bdkWCLEDKRc8fmBruh",
            "9z35ptatPpnzzfzCXB5k7aJ6jxg4Qyno5xXVMLC7oHAW",
            "Hb3evEtCB4ykLYYKFGkGgVuaCyrWFbXwe3jtkMSkq6Ve",
            "7cRPNt4EMSwvmwSrm379JhyG7s49CuCbCfnARzGykkzx",
            "AWnsFuKeTT3QDDinjCKsc5rM879iFQQz5rLq4JVNMhXF",
            "7Pzoefg48HurAC8ewRfgJgJmvGSasMYi5zHgQ67cxGTS",
            "8qhdhHUPxCqXDqhYiXDjjDnLAHctt7pEettHU6bTSXFB",
            "CoD1yTtv1rftMhmQJSRwqci547iKERgjPhqcxNfXSwZF",
            "9chT2PBZvM3TLfPQeHWyWREMUY8eLVp2uK5NnkGh3upt",
            "ymuFCM1UFr6AMmtHTHKbjm6GzmAaRVAFsGVwqWEhArj",
            "BWKw74TeWArB8TiEczNvihcsixadptis9sNwSTcLbwxo",
            "5yjxDcJzH1yaKYA6AH7GEgxtC6rdqy2kSCifjMLcmh8b",
            "CNDKBRwcagYkjfTPYB91DZsQwCQJfxrcywAdVQKnDJb1",
            "EKYai7Zh1qPtRtkkYJcCvdEPo2fXj3bfuTGoNUSJ4Z1m",
            "2ShQGNv3YNqABBSEVGwSk9s1UCaDMGGehdbnjiJ1HmYn",
            "D7rbZbEXZDMRepqDbvPorN26e1E6Qs5UoYnzLU9KqV3q",
            "33AiuucYaKGbCoSNBAuzdKCHZ5Svv7GS9DiZcVWbmygd",
            "ENDCh3U3BPg1Jw3pRKu9Vb4dUeRSFqqR5DAXhZZYpEgR",
            "7p9iJfB1ToLYv55JontFhAt1K5rA57oa9AWyrZRaRX4A",
            "CrMFwYDc3j4d9f9dogyDTx97raJJpAupntzVXm2bL8hH",
            "CHQ8Ti9L3z9aLfML1ULf78ZaGFwaTMA6GFcteBvMgmAG",
            "GGAtxXgYGP5YzxbhhnuYCwZJV24idnSp31XK81Lqu6vi",
            "HbJhdrHeagEKTKLiajwXSvgETRs652U3vcMHpQS5y8qr",
            "ApkrPbiWzEmkRj92g8tpzMNNXGkTogRNMQfESaUQAykv",
            "41PkE4UvqTeqNSjJ5S1oTGpt3bBHhcwaFBCJ6wL175NY",
            "ByREwqGG5AJeKgRSKavwbkyJsNfAx9sHEwzeh3HCwaWD",
            "2Lh9wSQnHkr3g3sgj5QJYqeRCQrYChX7ANwjjvs5rDGo",
            "7WxatmehVQjfXYMXoyFXnrRMCpDnMKcShFhNDXUioDpb",
            "27j5twe2g8Vqr6bGyEjX4dSPKA3s1hu4N1nm9u3gRsmc",
            "79RBFWfLKbJMNHzF6sqsAb7z9iXY1kyNFpuW6HJQQW4f",
            "BvPLZDd4cHfNeUy3F9tv8PQkJSWtwe5EyFpyTm81tQ9g",
            "GAd62dWPW36cxUPndsPwB52x1xjeRcJMuZ2SNdZTMzDR",
            "F8fj2myqZcw5CZy8yaNZDJXS2rzB7FL79maFBJM16WPu",
            "6W6iaKmdBNqyFrXmJqB5yrebr3KowYDs4F9Nide4ARYL",
            "82g9tPxdE3UgLiSoA4KkV6S7hXKdN4RDjcbBiqTwWSWw",
            "As83NX9BsHUx5RBuRWGWzByzXsSEoThVzcTpHrNkvUFt",
            "YkDo4ooVf3Sw79y84w3HCy7aGFd6UMfkGdgDMiHdprx",
            "2Ag3cXASMvi9aNTc3iPT2R31zVxQXeawhsx4Ukf2xTeH",
            "7Qm14zJHs8RVFjyWvauw945Usygjv3v3kP5nGrq8Hbij",
            "ChQYkyvvuAb25t6bWQzVpNytA25D2szmTMJusQLwUBpi",
            "DkFWVeEchx65Q9vymQjDjdfY472vV4JvkXompH5s4u6u",
            "74nxLDML7sZeFjeqLteu89kGxC18ZRdsSHsMP9q5BALP",
            "8PhY9FH6ZQT74nnBuxxoVs4nYk9NUWwguFfWrZHGiAwK",
            "BkwfZY9mrZyZRRDSFE6joFdDrvrXhGq2gpBXXecyjKAn",
            "5KdKapBaBy8BSCNst8qG4nxauE92KaBZDxJNXAGZpCGA",
            "HyXFTA4sq7NM4Bs49zPu25T1fzE5KXabafoiC4qAWjxR",
            "CmkfvxXTLihwp7eBm9HFtfGTvBHfK8cKPW99LEq1uR7c",
            "91efQM48sjFp5YPLRPVVFwxoER2YiYzy9BfhPeAk8oNS",
            "GYXWHy8iakBiffXh6NinMnxkuJkmNrMPry6zCPQdeaJ2",
            "7JivSGvY3ohqtGjmPZq59DqMJQ6FT73SeFPsH1nzypuP",
            "6fJ7t3uvmosPqXY51Qcwdc126miSnTF2bt7xgG5fyPgX",
            "6zPRRm2tjgUYPrnSboaxr2RvovB9y5vT9f4kYyzdbbri",
            "H9AcSjr1N9PX8pXGef98MS8sFL5p7bqLtPb71xeXo5Nn",
            "569xM2bZSTijhdB8u15G6e442PVq3PhcJUPFUZM9njvi",
            "BbuK8MkcZSJxx7VtRXB8Up8CqeURv3FRHzdzavPyKr1H",
            "6SXW9UNVdiUWJhw4RvfmovbkTEBeb8tvBzPHc8EeVWi",
            "6m8H4nKpm5fEQqKBaniRUYQfeaH7pjMkPPFiMnGXZEd8",
            "D8W6t585rTrYfhtQQTxozyG6w2oCLpBwLCy7rMsS1rNF",
            "7iZvgThSvJv95mkWnPPE6LENVcXTKfCVV2A4k3pZv9Te",
            "C7QuXVtEF3dm1ocF5nsigfPqRXkdH2oNr5683gs8SacC",
            "7SaJ5xEHTtWrFoJcNrNkJKb4pHJSDoMtzvcyHXJuhZRf",
            "A7AwVVu1y1bzz86LQrLbVtgC43k7LES6Lesc8ao7MGFP",
            "7rk54mPCRAg3jKCioCVK5wganTsGnn4ZYpCT9uTWzHi7",
            "AAaXsTU7hWFUY6UPTm2MfK8esLrh2P17M2e9kHSPwRMZ",
            "Hb2freG61xfMgmH6CUJDiyjG5zbbfcp9a6TQ2SJvs2Ur",
            "A6VbaGsFNZ3GBiin7XMRS2cwvbVpys8AKgnRmKSNvBwX",
            "6cNPxiBvD6xzA6UbKuNjuyRfbGpprdG3nRPdTYyZGwST",
            "HUmPDU5amyLXjpZJGwMf6YAa6W9LW2srJjcrSM3FXgUA",
            "5Wc3GkVnPmfRMgumstAEtT1yWYR7nBeyjYEQonz91bpb",
            "NvbSrT458e6LCeLq257rH73dZ7btcq1TPkQVsyL8ouT",
            "AG4harXetHAJLmKfQRTvW8uBBhjGDqbSvMQGTAhik4eE",
            "6nqWUFzjoJzuZ9FWF6iBjFwUpGX6mZrF25WaWsbwqekM",
            "9gGUAz4psCE2t3NFr8p64hgvXAcQGADtLmrNDTKMJPpC",
            "45t3M59UDEkeADxyGq1tJxwoUPQ5kUxeuLsusYBFkBah",
            "FmV9BnyyAGqVWudkV2S4hq7TGkUfoa8ku6UhKK1UN9zn",
            "8cQaC9iDd9pGQYcZfK61zgMitBvsW7218WzG55JhonsZ",
            "CC72FniNhUM41xSoA5h1idGxstrKXDqqhnYzfyBTSZDG",
            "Cy1zKgjXT5PREXaw72tu8sWjxwiTpmnwn1kzhMKaCrtb",
            "A4maCv1LM7rjpUzzM7JP4wg61VchwCovKiRqo3fbXSgF",
            "4ervEtFsXtxBJCshqaXbHBbv7xWz4XsSp6ptDPEJRq53",
            "93b5xNohGF24cbbGn7pw4QUKGLcrXdBVGw8gRsxTNcUx",
            "84GSk7hjwaUwX5wxmvnHiTtSnffPYP7dnmoP6ZJ5TnSR",
            "hLZNVV8EbeLCjkXCxfBePjAcuPSwCryWtiStoV7E4cU",
            "HZwUnXYy27TUWQucuuSM8dYJxxCgUMgfDf83vZNz7CKe",
            "CqVgxaQprGDZkBnAFNxKnwgaB5929CT1nSFxYjgUJz4H",
            "Ee7EsMJ6bYN99FKVTc9SEpX9HucyHg4AGuQpRuP9Ac4c",
            "Di5eqFCEonm8ABWfZph4fermaFbtrU5Jj2mrYdkKM1pT",
            "bfSBHwyL6PNjpeQEyrjsnbo7TzGRttbymvN74Y9czN3",
            "AgUfgteJ5mGWeuKZeUTedwvvu8jLaWGiRE2matEMoHEG",
            "xQob6CJEuPD23kbMENHWGWT7ho4Anwx4dpH7pBbX6MY",
            "6Cr9rtgY4c7H6DZzaCTgVpeFK8TpdcuhZXeZ2wqPhCD3",
            "CssW9uGzH22u9CUkft1xgqHgKCGCTrCL6Ma93wLY94dq",
            "GBEo8NoJAg5woWXXWXKT5xaWVUmxsqYgLkvyLkFDEgzY",
            "4WeKjRD14BGktxENsyE6hf8SBmmxY4F56V7TEMPrQ6nR",
            "7w3qaYPMAnbHnyWKqDzHVZcTLfJaDXhB2JdDUkZXuMho",
            "EUdiwR5oc6YfH3Y2AzQt5fUJAPB6F1kHyiqW8ZM5m5mg",
            "BWuHaUGmRKqGew1hffbCnfzkNNn4XLToyYp5zw8LiqSo",
            "EGLDBGNDaC1pr3iznmGDUSVLmFE8DGR1LruUHTwCFhhy",
        ];
        let pubkeys: Vec<Pubkey> = pool_addresses.iter().map(|p| { return Pubkey::from_str(*p).unwrap(); }).collect::<Vec<Pubkey>>();

        rpc.get_multiple_accounts(&pubkeys[0..100]).unwrap().iter().enumerate().for_each(|(index, market_account)| {
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

            if jupiter_invariant.ticks.len() == 0 {
                println!("fetched ticks array empty");
            }
            jupiter_invariant.ticks.iter().for_each(|(_, tick)| {
                println!("{:?}", tick);
            });

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

                let swap_leg_and_account_metas = jupiter_invariant.get_swap_leg_and_account_metas(&swap_params).unwrap();
                println!("{:?}", swap_leg_and_account_metas.account_metas);
            }
        });
    }

    #[test]
    fn test_price_limitation() {
        let let_max_price = JupiterInvariant::get_max_sqrt_price(1);
        assert_eq!(let_max_price, Price::new(9189293893553000000000000));
        let let_max_price = JupiterInvariant::get_max_sqrt_price(2);
        assert_eq!(let_max_price, Price::new(84443122262186000000000000));
        let let_max_price = JupiterInvariant::get_max_sqrt_price(5);
        assert_eq!(let_max_price, Price::new(65525554855399275000000000000));
        let let_max_price = JupiterInvariant::get_max_sqrt_price(10);
        assert_eq!(let_max_price, Price::new(65535383934512647000000000000));
        let let_max_price = JupiterInvariant::get_max_sqrt_price(100);
        assert_eq!(let_max_price, Price::new(65535383934512647000000000000));

        let let_min_price = JupiterInvariant::get_min_sqrt_price(1);
        assert_eq!(let_min_price, Price::new(108822289458000000000000));
        let let_min_price = JupiterInvariant::get_min_sqrt_price(2);
        assert_eq!(let_min_price, Price::new(11842290682000000000000));
        let let_min_price = JupiterInvariant::get_min_sqrt_price(5);
        assert_eq!(let_min_price, Price::new(15261221000000000000));
        let let_min_price = JupiterInvariant::get_min_sqrt_price(10);
        assert_eq!(let_min_price, Price::new(15258932000000000000));
        let let_min_price = JupiterInvariant::get_min_sqrt_price(100);
        assert_eq!(let_min_price, Price::new(15258932000000000000));
    }
}
