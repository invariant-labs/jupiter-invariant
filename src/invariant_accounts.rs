use anchor_lang::prelude::*;
use anyhow::{Error, Result};
use invariant_types::{SEED, STATE_SEED};

use crate::{swap_simulation::InvariantSwapResult, JupiterInvariant};

#[derive(Clone)]
pub struct InvariantSwapParams<'a> {
    pub invariant_swap_result: &'a InvariantSwapResult,
    pub owner: Pubkey,
    pub source_mint: Pubkey,
    pub destination_mint: Pubkey,
    pub source_account: Pubkey,
    pub destination_account: Pubkey,
    pub referral_fee: Option<Pubkey>,
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
