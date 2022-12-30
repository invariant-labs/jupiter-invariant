use std::collections::HashMap;
use anchor_lang::prelude::Pubkey;
use jupiter_core::amm::{Amm, KeyedAccount, Quote, QuoteParams, SwapLegAndAccountMetas, SwapParams};

#[derive(Clone, Debug)]
pub struct JupiterInvariant {}

impl JupiterInvariant {
    pub fn new_from_keyed_account(keyed_account: &KeyedAccount) -> Result<Self, ()> {
        Ok(Self {})
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
        todo!()
    }

    fn update(&mut self, accounts_map: &HashMap<Pubkey, Vec<u8>>) -> anyhow::Result<()> {
        todo!()
    }

    fn quote(&self, quote_params: &QuoteParams) -> anyhow::Result<Quote> {
        todo!()
    }

    fn get_swap_leg_and_account_metas(&self, swap_params: &SwapParams) -> anyhow::Result<SwapLegAndAccountMetas> {
        todo!()
    }

    fn clone_amm(&self) -> Box<dyn Amm + Send + Sync> {
        todo!()
    }
}