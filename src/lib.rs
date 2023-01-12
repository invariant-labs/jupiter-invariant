use std::borrow::Borrow;
use std::cell::RefCell;
use anchor_lang::prelude::Pubkey;
use anchor_lang::{AnchorDeserialize, Key};
use invariant_types::structs::{Pool, Tick, Tickmap};
use jupiter_core::amm::{
    Amm, KeyedAccount, Quote, QuoteParams, SwapLegAndAccountMetas, SwapParams,
};
use solana_sdk::pubkey;
use std::collections::HashMap;
use invariant_types::decimals::*;
use invariant_types::errors::InvariantErrorCode;
use invariant_types::math::{compute_swap_step, cross_tick, get_closer_limit, is_enough_amount_to_push_price, SwapResult};
use solana_client::rpc_client::RpcClient;

pub const ANCHOR_DISCRIMINATOR_SIZE: usize = 8;
pub const TICK_LIMIT: i32 = 44364;
pub const TICKMAP_SIZE: i32 = 2 * TICK_LIMIT - 1;
pub const PROGRAM_ID: Pubkey = pubkey!("HyaB3W9q6XdA5xwpU4XnSZV94htfmbmqJXZcEbRaJutt");
// pub const PROGRAM_ID: Pubkey = pubkey!("9aiirQKPZ2peE9QrXYmsbTtR7wSDJi2HkQdHuaMpTpei"); // Devnet
pub const TICK_CROSSES_PER_IX: usize = 19;
pub const MIN_PRICE: Price = Price { v: 15258932000000000000 };
pub const MAX_PRICE: Price = Price { v: 65535383934512647000000000000 };

#[derive(Clone, Default)]
pub struct JupiterInvariant {
    market_key: Pubkey,
    label: String,
    pool: Pool,
    tickmap: Tickmap,
    ticks: HashMap<Pubkey, Tick>,
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

    fn fetch_accounts_map(rpc: &RpcClient, accounts_to_update: Vec<Pubkey>) -> HashMap<Pubkey, Vec<u8>> {
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
            &[b"tickv1", self.market_key.key().as_ref(), &i.to_le_bytes()],
            &PROGRAM_ID,
        );
        pubkey
    }

    fn get_ticks_addresses_around(&self) -> Vec<Pubkey> {
        // self.tickmap.bitmap.iter().for_each(|b| {
        //     if *b != 0 {
        //         println!("find non-zero byte");
        //     }
        // });
        let above_addresses = self
            .find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::UP);
        // println!("above: {:?}", above_addresses);

        let below_addresses = self
            .find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::DOWN);
        // println!("below: {:?}", below_addresses);

        let all_indexes = [below_addresses, above_addresses].concat();
        // println!("all ticks indexes = {:?}", all_indexes);
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
        let by_amount_in = true;
        let x_to_y = quote_params.input_mint.eq(&self.pool.token_x);
        let mut sqrt_price_limit: Price = (if x_to_y { MIN_PRICE } else { MAX_PRICE }).clone();

        let calculate_amount_out = || -> Result<u64, InvariantErrorCode> {
            let mut pool: RefCell<Pool> = RefCell::new(self.pool.clone());
            let mut _ticks: RefCell<HashMap<Pubkey, Tick>> = RefCell::new(self.ticks.clone());
            let mut _tickmap: RefCell<Tickmap> = RefCell::new(self.tickmap.clone());
            let mut pool = pool.borrow_mut();
            let mut remaining_amount = TokenAmount::new(in_amount);
            let mut total_amount_in: TokenAmount = TokenAmount::new(0);
            let mut total_amount_out: TokenAmount = TokenAmount::new(0);
            while !remaining_amount.is_zero() {
                // not_enough_liquidity if failed
                let (swap_limit, limiting_tick) =
                    get_closer_limit(sqrt_price_limit, x_to_y, pool.current_tick_index, pool.tick_spacing, &self.tickmap).unwrap();
                let result: SwapResult = compute_swap_step(pool.sqrt_price, swap_limit, pool.liquidity, remaining_amount, by_amount_in, pool.fee);
                remaining_amount -= result.amount_in + result.fee_amount;
                pool.sqrt_price = result.next_price_sqrt;
                total_amount_in += result.amount_in + result.fee_amount;
                total_amount_out += result.amount_out;
                // Fail if price would go over swap limit
                if { pool.sqrt_price } == sqrt_price_limit && !remaining_amount.is_zero() {
                    return Err(InvariantErrorCode::PriceLimitReached.into());
                }

                // crossing tick
                // trunk-ignore(clippy/unnecessary_unwrap)
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
                    }
                    // set tick to limit (below if price is going down, because current tick should always be below price)
                    // pool.current_tick_index = if x_to_y && is_enough_amount_to_cross {
                    //     tick_index.checked_sub(pool.tick_spacing as i32).unwrap()
                    // } else {
                    //     tick_index
                    // };
                    // } else {
                    //     assert!(
                    //         pool.current_tick_index
                    //             .checked_rem(pool.tick_spacing.into())
                    //             .unwrap()
                    //             == 0,
                    //         "tick not divisible by spacing"
                    //     );
                    //     pool.current_tick_index =
                    //         get_tick_at_sqrt_price(result.next_price_sqrt, pool.tick_spacing);
                }
            }
            Ok(total_amount_out.0)
        };

        let result = calculate_amount_out();
        match result {
            Ok(out_amount) => {
                Ok(Quote {
                    out_amount,
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
        let _ = swap_params;
        todo!()
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

        // get accounts to update
        let accounts_to_update = jupiter_invariant.get_accounts_to_update();
        // get data from accounts
        let accounts_map = JupiterInvariant::fetch_accounts_map(&rpc, accounts_to_update);
        // update state
        jupiter_invariant.update(&accounts_map).unwrap();

        let accounts_to_update = jupiter_invariant.get_accounts_to_update();
        let accounts_map = JupiterInvariant::fetch_accounts_map(&rpc, accounts_to_update);
        jupiter_invariant.update(&accounts_map).unwrap();

        jupiter_invariant.ticks.iter().for_each(|(_, tick)| {
            println!("{:?}", tick);
        });
    }

    #[test]
    fn test_fetch_all_poo() {
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
        // println!("len = {:?}", pool_addresses.len());
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
            let accounts_map = JupiterInvariant::fetch_accounts_map(&rpc, accounts_to_update);
            jupiter_invariant.update(&accounts_map).unwrap();
            let accounts_to_update = jupiter_invariant.get_accounts_to_update();
            let accounts_map = JupiterInvariant::fetch_accounts_map(&rpc, accounts_to_update);
            jupiter_invariant.update(&accounts_map).unwrap();

            if jupiter_invariant.ticks.len() == 0 {
                println!("fetched ticks array empty");
            }
            jupiter_invariant.ticks.iter().for_each(|(_, tick)| {
                println!("{:?}", tick);
            });
        });
    }
}
