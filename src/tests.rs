#[cfg(test)]
mod tests {
    use anchor_lang::prelude::Pubkey;
    use solana_client::rpc_client::RpcClient;
    use solana_sdk::pubkey;

    use jupiter_core::amm::{Amm, KeyedAccount, QuoteParams, SwapParams};
    use std::str::FromStr;

    use crate::JupiterInvariant;

    const RPC_MAINNET_CLINET: &str = "https://api.mainnet-beta.solana.com";

    #[test]
    fn test_jupiter_invariant() {
        use anchor_lang::prelude::*;
        use solana_client::rpc_client::RpcClient;

        const USDC_USDT_MARKET: Pubkey = pubkey!("BRt1iVYDNoohkL1upEb8UfHE8yji6gEDAmuN9Y4yekyc");
        const USDC: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
        const USDT: Pubkey = pubkey!("Es9vMFrzaCERmJfrF4H2FYD4KCoNkY11McCe8BenwNYB");
        let rpc_url = std::env::args()
            .filter(|arg| arg.starts_with("rpc="))
            .map(|arg| arg.split_at(4).1.to_string())
            .next()
            .unwrap_or_else(|| RPC_MAINNET_CLINET.to_string());
        let rpc = RpcClient::new(rpc_url);
        let mut input_mint = (USDC, stringify!(USDC));
        let mut output_mint = (USDT, stringify!(USDT));
        if let Some(_) = std::env::args().find(|arg| arg.starts_with("dir=reversed")) {
            (input_mint, output_mint) = (output_mint, input_mint);
        }

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
            input_mint: input_mint.0,
            output_mint: output_mint.0,
        };
        let result = jupiter_invariant.quote(&quote).unwrap();

        println!("insufficient liquidity: {:?}", result.not_enough_liquidity);
        println!(
            "input amount: {:.6} {}",
            result.in_amount as f64 / 10u64.pow(6) as f64,
            input_mint.1
        );
        println!(
            "output amount: {:.6} {}",
            result.out_amount as f64 / 10u64.pow(6) as f64,
            output_mint.1
        );
        println!(
            "fee amount: {:.6} {}",
            result.fee_amount as f64 / 10u64.pow(6) as f64,
            input_mint.1
        );

        let _swap_leg_and_account_metas = jupiter_invariant
            .get_swap_leg_and_account_metas(&SwapParams {
                source_mint: quote.input_mint,
                destination_mint: quote.output_mint,
                user_destination_token_account: Pubkey::new_unique(),
                user_source_token_account: Pubkey::new_unique(),
                user_transfer_authority: Pubkey::new_unique(),
                open_order_address: None,
                quote_mint_to_referrer: None,
                in_amount: quote.in_amount,
            })
            .unwrap();
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
        let pubkeys: Vec<Pubkey> = pool_addresses
            .iter()
            .map(|p| {
                return Pubkey::from_str(*p).unwrap();
            })
            .collect::<Vec<Pubkey>>();

        rpc.get_multiple_accounts(&pubkeys)
            .unwrap()
            .iter()
            .enumerate()
            .for_each(|(index, market_account)| {
                let key: Pubkey = pubkeys.get(index).unwrap().to_owned();
                let account = market_account.to_owned().unwrap();
                let mut jupiter_invariant =
                    JupiterInvariant::new_from_keyed_account(&KeyedAccount {
                        key,
                        account,
                        params: None,
                    })
                    .unwrap();
                let accounts_to_update = jupiter_invariant.get_accounts_to_update();
                let accounts_map = JupiterInvariant::fetch_accounts(&rpc, accounts_to_update);
                jupiter_invariant.update(&accounts_map).unwrap();
                let accounts_to_update = jupiter_invariant.get_accounts_to_update();
                let accounts_map = JupiterInvariant::fetch_accounts(&rpc, accounts_to_update);
                jupiter_invariant.update(&accounts_map).unwrap();

                let (user_transfer_authority, user_token_x_account, user_token_y_account) = (
                    Pubkey::new_unique(),
                    Pubkey::new_unique(),
                    Pubkey::new_unique(),
                );

                for i in 0..2 {
                    let x_to_y = i % 2 == 0;

                    let (
                        source_mint,
                        user_source_token_account,
                        destination_mint,
                        user_destination_token_account,
                    ) = if x_to_y {
                        (
                            jupiter_invariant.pool.token_x,
                            user_token_x_account,
                            jupiter_invariant.pool.token_y,
                            user_token_y_account,
                        )
                    } else {
                        (
                            jupiter_invariant.pool.token_y,
                            user_token_y_account,
                            jupiter_invariant.pool.token_x,
                            user_token_x_account,
                        )
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
                    let _swap_leg_and_account_metas = jupiter_invariant
                        .get_swap_leg_and_account_metas(&swap_params)
                        .unwrap();
                }
            });
    }
}
