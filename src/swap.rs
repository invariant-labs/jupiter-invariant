use std::cell::RefCell;

use invariant_types::{
    decimals::{Decimal, Price, TokenAmount},
    log::get_tick_at_sqrt_price,
    math::{
        compute_swap_step, cross_tick, get_closer_limit, get_max_sqrt_price, get_min_sqrt_price,
        is_enough_amount_to_push_price,
    },
    structs::TICK_CROSSES_PER_IX,
    MAX_VIRTUAL_CROSS,
};
use jupiter_core::amm::QuoteParams;

use crate::JupiterInvariant;

pub struct InvariantSimulationParams {
    pub in_amount: u64,
    pub x_to_y: bool,
    pub by_amount_in: bool,
    pub sqrt_price_limit: Price,
}

#[derive(Clone, Default)]
pub struct InvariantSwapResult {
    pub in_amount: u64,
    pub out_amount: u64,
    pub fee_amount: u64,
    pub starting_sqrt_price: Price,
    pub ending_sqrt_price: Price,
    pub crossed_ticks: Vec<i32>,
    pub virtual_cross_counter: u16,
    pub global_insufficient_liquidity: bool,
    pub ticks_accounts_outdated: bool,
}

impl InvariantSwapResult {
    pub fn is_not_enough_liquidity(&self) -> bool {
        // since "is_referral" is not specified in the quote parameters, we pessimistically assume that the referral is always used
        self.ticks_accounts_outdated || self.is_not_enough_liquidity_referral(true)
    }

    fn is_exceeded_cu_referral(&self, is_referral: bool) -> bool {
        let crossed_amount = self.crossed_ticks.len();
        let mut max_cross = TICK_CROSSES_PER_IX;
        if is_referral {
            max_cross -= 1;
        }
        let is_exceeded_by_account_size = crossed_amount > max_cross;
        let is_exceeded_by_compute_units =
            crossed_amount == max_cross && self.virtual_cross_counter > MAX_VIRTUAL_CROSS;

        is_exceeded_by_account_size || is_exceeded_by_compute_units
    }

    fn is_not_enough_liquidity_referral(&self, is_referral: bool) -> bool {
        self.is_exceeded_cu_referral(is_referral) || self.global_insufficient_liquidity
    }
}

impl JupiterInvariant {
    pub fn quote_to_invariant_params(
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

    pub fn simulate_invariant_swap(
        &self,
        invariant_simulation_params: &InvariantSimulationParams,
    ) -> Result<InvariantSwapResult, String> {
        let InvariantSimulationParams {
            in_amount,
            x_to_y,
            sqrt_price_limit,
            by_amount_in,
        } = *invariant_simulation_params;

        let (mut pool, ticks, tickmap, starting_sqrt_price) = (
            &mut self.pool.clone(),
            &self.ticks.clone(),
            &self.tickmap.clone(),
            self.pool.sqrt_price,
        );
        let (mut remaining_amount, mut total_amount_in, mut total_amount_out, mut total_fee_amount) = (
            TokenAmount::new(in_amount),
            TokenAmount::new(0),
            TokenAmount::new(0),
            TokenAmount::new(0),
        );
        let (
            mut crossed_ticks,
            mut virtual_cross_counter,
            mut global_insufficient_liquidity,
            mut ticks_accounts_outdated,
        ) = (Vec::new(), 0u16, false, false);

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

            let result = compute_swap_step(
                pool.sqrt_price,
                swap_limit,
                pool.liquidity,
                remaining_amount,
                by_amount_in,
                pool.fee,
            )
            .map_err(|e| {
                let (formatted, _, _) = e.get();
                formatted
            })?;

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
                )
                .map_err(|e| {
                    let (formatted, _, _) = e.get();
                    formatted
                })?;

                if initialized {
                    let tick_address = self.tick_index_to_address(tick_index);
                    let tick = match ticks.get(&tick_address) {
                        Some(tick) => RefCell::new(*tick),
                        None => {
                            ticks_accounts_outdated = true;
                            break;
                        }
                    };
                    let mut tick = tick.borrow_mut();

                    // crossing tick
                    if !x_to_y || is_enough_amount_to_cross {
                        cross_tick(&mut tick, pool)
                            .map_err(|_| "Internal Invariant Error: Cross tick".to_string())?;
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
                    return Err("Internal Invariant Error: Invalid tick".to_string());
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
            starting_sqrt_price,
            ending_sqrt_price: pool.sqrt_price,
            crossed_ticks,
            virtual_cross_counter,
            global_insufficient_liquidity,
            ticks_accounts_outdated,
        })
    }
}
