pub mod accounts;
pub mod swap;
mod tests;
pub mod utiles;

use anchor_lang::prelude::*;
use anyhow::Result;
use std::collections::HashMap;

use accounts::{InvariantSwapAccounts, InvariantSwapParams};
use invariant_types::structs::{Pool, Tick, Tickmap};
use invariant_types::ID;
use swap::InvariantSwapResult;

use jupiter::jupiter_override::{Swap, SwapLeg};
use jupiter_core::amm::{
    Amm, KeyedAccount, Quote, QuoteParams, SwapLegAndAccountMetas, SwapParams,
};

pub type Ticks = HashMap<Pubkey, Tick>;

#[derive(Clone, Default)]
pub struct JupiterInvariant {
    pub program_id: Pubkey,
    pub market_key: Pubkey,
    pub label: String,
    pub pool: Pool,
    pub tickmap: Tickmap,
    pub ticks: Ticks,
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
            .collect::<Result<Ticks>>()?;

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
            quote_mint_to_referrer,
            ..
        } = swap_params;

        let referral_fee: Option<Pubkey> = match quote_mint_to_referrer {
            Some(referral) => referral.get(&source_mint).copied(),
            _ => None,
        };

        let quote_params = QuoteParams {
            in_amount: *in_amount,
            input_mint: *source_mint,
            output_mint: *destination_mint,
        };
        let invarinat_simulation_params = self.quote_to_invarinat_params(&quote_params)?;
        let invariant_swap_result = self
            .simulate_invariant_swap(&invarinat_simulation_params)
            .map_err(|e| anyhow::anyhow!("Simulation error: {}", e))?;

        if invariant_swap_result.ticks_accounts_outdated {
            return Err(anyhow::anyhow!("ticks accounts outdated"));
        }
        if invariant_swap_result.is_not_enoght_liquidity() {
            return Err(anyhow::anyhow!("insufficient liquidity"));
        }

        let invariant_swap_params = InvariantSwapParams {
            invariant_swap_result: &invariant_swap_result,
            owner: *user_transfer_authority,
            source_mint: *source_mint,
            destination_mint: *destination_mint,
            source_account: *user_source_token_account,
            destination_account: *user_destination_token_account,
            referral_fee,
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
