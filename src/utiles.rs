use std::collections::HashMap;

use anchor_lang::Key;
use anchor_lang::{prelude::Pubkey, AnchorDeserialize};
use invariant_types::decimals::{BigOps, Decimal, Price, U256};
use invariant_types::{
    structs::{TICKMAP_SIZE, TICK_CROSSES_PER_IX, TICK_LIMIT},
    ANCHOR_DISCRIMINATOR_SIZE, TICK_SEED,
};
use rust_decimal::prelude::FromPrimitive;
use solana_client::rpc_client::RpcClient;

use crate::JupiterInvariant;

enum PriceDirection {
    UP,
    DOWN,
}

impl JupiterInvariant {
    pub const PRICE_IMPACT_ACCURACY: u128 = 1_000_000_000_000u128;

    pub fn deserialize<T>(data: &[u8]) -> anyhow::Result<T>
    where
        T: AnchorDeserialize,
    {
        T::try_from_slice(Self::extract_from_anchor_account(data))
            .map_err(|e| anyhow::anyhow!("Error deserializing account data: {:?}", e))
    }

    pub fn fetch_accounts(
        rpc: &RpcClient,
        accounts_to_update: Vec<Pubkey>,
    ) -> HashMap<Pubkey, Vec<u8>> {
        rpc.get_multiple_accounts(&accounts_to_update)
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

    pub fn tick_indexes_to_addresses(&self, indexes: &[i32]) -> Vec<Pubkey> {
        let pubkeys: Vec<Pubkey> = indexes
            .iter()
            .map(|i| self.tick_index_to_address(*i))
            .collect();
        pubkeys
    }

    pub fn tick_index_to_address(&self, i: i32) -> Pubkey {
        let (pubkey, _) = Pubkey::find_program_address(
            &[
                TICK_SEED.as_bytes(),
                self.market_key.key().as_ref(),
                &i.to_le_bytes(),
            ],
            &self.program_id,
        );
        pubkey
    }

    pub fn get_ticks_addresses_around(&self) -> Vec<Pubkey> {
        let above_indexes = self.find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::UP);
        let below_indexes =
            self.find_closest_tick_indexes(TICK_CROSSES_PER_IX, PriceDirection::DOWN);
        let all_indexes = [below_indexes, above_indexes].concat();

        self.tick_indexes_to_addresses(&all_indexes)
    }

    pub fn ticks_accounts_outdated(&self) -> bool {
        let ticks_addresses = self.get_ticks_addresses_around();

        ticks_addresses
            .iter()
            .any(|address| !self.ticks.contains_key(address))
    }

    fn extract_from_anchor_account(data: &[u8]) -> &[u8] {
        data.split_at(ANCHOR_DISCRIMINATOR_SIZE).1
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
        let current_index = current / tick_spacing + TICK_LIMIT;
        let (mut above, mut below, mut reached_limit) = (current_index + 1, current_index, false);

        while !reached_limit && found.len() < amount_limit {
            match direction {
                PriceDirection::UP => {
                    let value_above: u8 =
                        *tickmap.get((above / 8) as usize).unwrap() & (1 << (above % 8));
                    if value_above != 0 {
                        found.push(above);
                    }
                    reached_limit = above >= TICKMAP_SIZE;
                    above += 1;
                }
                PriceDirection::DOWN => {
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

        found
            .iter()
            .map(|i: &i32| (i - TICK_LIMIT) * tick_spacing)
            .collect()
    }

    pub fn calculate_price_impact(
        starting_sqrt_price: Price,
        ending_sqrt_price: Price,
    ) -> Result<rust_decimal::Decimal, &'static str> {
        let accuracy = U256::from(Self::PRICE_IMPACT_ACCURACY);
        let starting_price = U256::from(starting_sqrt_price.big_mul(starting_sqrt_price).get());
        let ending_price = U256::from(ending_sqrt_price.big_mul(ending_sqrt_price).get());

        let (numerator, denominator) = match starting_price > ending_price {
            true => (ending_price, starting_price),
            false => (starting_price, ending_price),
        };
        let price_quote = accuracy
            .checked_mul(numerator)
            .and_then(|result| result.checked_div(denominator))
            .ok_or("mul/div overflow")?;

        let price_impact_decimal = accuracy.checked_sub(price_quote).ok_or("sub overflow")?;
        let price_impact_pct = f64::from_u128(price_impact_decimal.as_u128())
            .ok_or_else(|| "converting price impact to f64")?
            / f64::from_u128(Self::PRICE_IMPACT_ACCURACY)
                .ok_or_else(|| "converting accuracy to f64")?;

        Ok(rust_decimal::Decimal::from_f64(price_impact_pct)
            .ok_or_else(|| "converting to rust_decimal")?)
    }
}

#[cfg(test)]
mod tests {
    use invariant_types::decimals::{Decimal, Factories, Price};
    use rust_decimal::prelude::FromPrimitive;

    use crate::JupiterInvariant;

    #[test]
    fn test_calculate_price_impact() {
        {
            // 1 -> 6
            {
                let a = Price::from_integer(1);
                let b = Price::new(2449489742783178098197284);

                let result = JupiterInvariant::calculate_price_impact(a, b).unwrap();
                let reversed_result = JupiterInvariant::calculate_price_impact(a, b).unwrap();

                // real:        0.8(3)
                // expected     0.833333333334
                assert_eq!(
                    result,
                    rust_decimal::Decimal::from_f64(0.833333333334).unwrap()
                );
                assert_eq!(
                    reversed_result,
                    rust_decimal::Decimal::from_f64(0.833333333334).unwrap()
                );
            }
            // 55000 -> 55000.4
            {
                let a = Price::new(234520787991171477728281505u128);
                let b = Price::new(234521640792486355143954683u128);

                let result: rust_decimal::Decimal =
                    JupiterInvariant::calculate_price_impact(a, b).unwrap();

                // real:        0.0000072726743...
                // expected     0.000007272675
                assert_eq!(
                    result,
                    rust_decimal::Decimal::from_f64(0.000007272675).unwrap()
                );
            }
            // 1 -> 0.9999
            {
                let a = Price::from_integer(1);
                let b = Price::new(999949998749937496093476);

                let result: rust_decimal::Decimal =
                    JupiterInvariant::calculate_price_impact(a, b).unwrap();

                // real:        0.0001
                // expected     0.000100000001
                assert_eq!(
                    result,
                    rust_decimal::Decimal::from_f64(0.000100000001).unwrap()
                );
            }
            // 0.2 -> 1.3
            {
                let a = Price::new(447213595499957939281834);
                let b = Price::new(1140175425099137979136049);

                let result: rust_decimal::Decimal =
                    JupiterInvariant::calculate_price_impact(a, b).unwrap();

                // real:        0.8461538461538...
                // expected     0.846153846154
                assert_eq!(
                    result,
                    rust_decimal::Decimal::from_f64(0.846153846154).unwrap()
                );
            }
            // 0.000197 -> 0.000246
            {
                let a = Price::new(15684387141358121934184);
                let b = Price::new(14035668847618199630519);

                let result: rust_decimal::Decimal =
                    JupiterInvariant::calculate_price_impact(a, b).unwrap();

                // real:        0.199186991869...
                // expected     0.19918699187
                assert_eq!(
                    result,
                    rust_decimal::Decimal::from_f64(0.19918699187).unwrap()
                );
            }
        }
        // EDGE CASES
        {
            let max_sqrt_price = Price::new(65535383934512647000000000000);
            let almost_max_sqrt_price = max_sqrt_price - Price::new(1);
            let min_sqrt_price = Price::new(15258932000000000000);
            let almost_min_sqrt_price = min_sqrt_price + Price::new(1);

            // min_price -> max_price
            // 4294886547.443978352291489402946609
            // 0.000000000232
            {
                let result =
                    JupiterInvariant::calculate_price_impact(min_sqrt_price, max_sqrt_price)
                        .unwrap();

                // real:        0.99999999999999999994...
                // expected     1
                assert_eq!(result, rust_decimal::Decimal::from_f64(1.).unwrap());
            }
            // min_sqrt_price -> almost_max_sqrt_price
            {
                let result =
                    JupiterInvariant::calculate_price_impact(min_sqrt_price, almost_min_sqrt_price)
                        .unwrap();

                assert_eq!(result, rust_decimal::Decimal::from_f64(0.).unwrap());
            }
            // max_sqrt_price -> almost_max_sqrt_price
            {
                let result =
                    JupiterInvariant::calculate_price_impact(max_sqrt_price, almost_max_sqrt_price)
                        .unwrap();

                assert_eq!(
                    result,
                    rust_decimal::Decimal::from_f64(0.000000000001).unwrap()
                );
            }
        }
    }
}
