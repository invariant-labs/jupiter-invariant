use anchor_lang::prelude::Pubkey;
use jupiter_core::amm::{
    Amm, KeyedAccount, Quote, QuoteParams, SwapLegAndAccountMetas, SwapParams,
};
use solana_sdk::entrypoint::deserialize;
use solana_sdk::pubkey;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct JupiterInvariant {
    label: String,
}

impl JupiterInvariant {
    pub fn new_from_keyed_account(keyed_account: &KeyedAccount) -> Result<Self, ()> {
        Ok(Self {
            label: String::from("Invariant"),
        })
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

#[test]
fn test_jupiter_invariant() {
    use anchor_lang::prelude::*;
    use solana_client::rpc_client::RpcClient;

    const USDC_USDT_MARKET: Pubkey = pubkey!("BRt1iVYDNoohkL1upEb8UfHE8yji6gEDAmuN9Y4yekyc");
    let rpc = RpcClient::new("https://tame-ancient-mountain.solana-mainnet.quiknode.pro/6a9a95bf7bbb108aea620e7ee4c1fd5e1b67cc62");
    let pool_data = rpc.get_account(&USDC_USDT_MARKET).unwrap();
    println!("{:?}", pool_data);
}
