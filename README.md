# Jupiter Invariant Integration

Implements of the `Amm` trait defined [here](https://github.com/jup-ag/rust-amm-implementation).

The following command will run the test:
```shell
cargo test test_jupiter_invariant -- --nocapture
```
The result is quote for selling 1 USDC on USDC/USDT Invariant market. Example response:
```
insufficient liquidity: false
input amount: 1.000000 USDC
output amount: 1.000056 USDT
fee amount: 0.000010 USDC
```
The RPC provided by Solana on the mainnet is used by default. If you encounter connection issues with the RPC, you can manually specify it using the following command:
```bash
cargo test test_jupiter_invariant -- --nocapture "rpc=https://your-rpc.com/..."
```

