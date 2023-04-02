# Jupiter Invariant Integration

Implements of the `Amm` trait defined [here](https://github.com/jup-ag/rust-amm-implementation).

## Tests

To run the test, use the following command:
```shell
cargo test test_jupiter_invariant -- --nocapture
```
The result is a quote for selling 1 USDC on the USDC/USDT Invariant market. Here's an example response:
```
insufficient liquidity: false
input amount: 1.000000 USDC
output amount: 1.000056 USDT
fee amount: 0.000010 USDC
```

If you want to reverse the swap direction, use this command:
```shell
cargo test test_jupiter_invariant -- --nocapture "dir=reversed"
```

The RPC provided by Solana on the mainnet is used by default. If you encounter connection issues with the RPC, you can manually specify it using the following command:
```bash
cargo test test_jupiter_invariant -- --nocapture "rpc=https://your-rpc.com/..."
```

You can combine both parameters:
```bash
cargo test test_jupiter_invariant -- --nocapture "dir=reversed" "rpc=https://your-rpc.com/..."
```

## Accounts Referesing

It's crucial to take into account how frequently accounts are updated when a library client relies on retrieving every required account at once during the quote action cycle. Below are the recommendations for updating accounts in the quote cycle.

The invariant design differs from the classic AMM by featuring a different architecture that makes it impossible to determine all the required accounts before fetching a tickmap account. Consequently, a minimum of two chained fetches of account batches is needed. The first fetch should contain all accounts except for tick accounts, while the second fetch should contain tick accounts. It should be noted that updating all accounts twice will yield the same outcome.

In the case of high-frequency updates (few seconds or less), a single fetch of accounts is sufficient since the tickmap rarely changes. However, a double initial fetch is still required. In the case of a single account update after initialization, the quote will return an insufficient liquidity result unless the swap amount is small enough that it does not cross any ticks.

If the frequency of account refresh is lower, it is recommended to check whether accounts are outdated after updating accounts. For this purpose, the JupiterInvariant::get_accounts_to_update() function has been added. Below is an example code snippet that updates accounts until the tick accounts are up-to-date:
```rust
        // update market data
        let accounts_to_update = jupiter_invariant.get_accounts_to_update();
        let accounts_map = JupiterInvariant::fetch_accounts(&rpc, accounts_to_update);
        jupiter_invariant.update(&accounts_map).unwrap();

        let mut accounts_outdated = jupiter_invariant.ticks_accounts_outdated();
        // update once again due to fetch accounts on a non-initialized tickmap.
        while accounts_outdated {
            let accounts_to_update = jupiter_invariant.get_accounts_to_update();
            let accounts_map = JupiterInvariant::fetch_accounts(&rpc, accounts_to_update);
            jupiter_invariant.update(&accounts_map).unwrap();
            accounts_outdated = jupiter_invariant.ticks_accounts_outdated();
        }
```