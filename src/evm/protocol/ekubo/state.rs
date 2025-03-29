use std::{any::Any, collections::HashMap, fmt::Debug};

use alloy_primitives::Address;
use evm_ekubo_sdk::{
    math::{
        tick::{MAX_TICK, MIN_TICK},
        uint::U256,
    },
    quoting::types::{NodeKey, Tick, TokenAmount},
};
use num_bigint::BigUint;
use tycho_core::{dto::ProtocolStateDelta, Bytes};

use super::{
    pool::{base::BasePool, oracle::OraclePool, EkuboPool},
    tick::ticks_from_attributes,
};
use crate::{
    evm::protocol::u256_num::u256_to_f64,
    models::{Balances, Token},
    protocol::{
        errors::{SimulationError, TransitionError},
        models::GetAmountOutResult,
        state::ProtocolSim,
    },
};

#[enum_delegate::implement(EkuboPool)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EkuboState {
    Base(BasePool),
    Oracle(OraclePool),
}

impl EkuboState {
    pub fn set_tick(&mut self, tick: Tick) -> Result<(), String> {
        match self {
            Self::Base(p) => {
                p.set_tick(tick);

                Ok(())
            }
            Self::Oracle(p) => {
                let idx = tick.index;

                if ![MIN_TICK, MAX_TICK].contains(&idx) {
                    return Err(format!(
                        "oracle must be full-range but passed tick has index {idx}"
                    ));
                }

                p.set_liquidity(tick.liquidity_delta.unsigned_abs());

                Ok(())
            }
        }
    }
}

fn sqrt_price_q128_to_f64(x: U256, (token0_decimals, token1_decimals): (usize, usize)) -> f64 {
    let token_correction = 10f64.powi(token0_decimals as i32 - token1_decimals as i32);

    let price = u256_to_f64(alloy_primitives::U256::from_limbs(x.0)) / 2.0f64.powi(128);
    price.powi(2) * token_correction
}

impl ProtocolSim for EkuboState {
    fn fee(&self) -> f64 {
        self.key().config.fee as f64 / (2f64.powi(64))
    }

    fn spot_price(&self, base: &Token, quote: &Token) -> Result<f64, SimulationError> {
        let sqrt_ratio = self.sqrt_ratio();
        let (base_decimals, quote_decimals) = (base.decimals, quote.decimals);

        Ok(if base < quote {
            sqrt_price_q128_to_f64(sqrt_ratio, (base_decimals, quote_decimals))
        } else {
            1.0f64 / sqrt_price_q128_to_f64(sqrt_ratio, (quote_decimals, base_decimals))
        })
    }

    // TODO Need a timestamp here for the Oracle pool (and TWAMM in the future)
    fn get_amount_out(
        &self,
        amount_in: BigUint,
        token_in: &Token,
        _token_out: &Token,
    ) -> Result<GetAmountOutResult, SimulationError> {
        let token_amount = TokenAmount {
            token: U256::from_big_endian(&token_in.address),
            amount: amount_in.try_into().map_err(|_| {
                SimulationError::InvalidInput("amount in must fit into a i128".to_string(), None)
            })?,
        };

        let quote = match self {
            Self::Base(p) => p.quote(token_amount),
            Self::Oracle(p) => p.quote(token_amount),
        }?;

        Ok(GetAmountOutResult {
            amount: BigUint::try_from(quote.calculated_amount).map_err(|_| {
                SimulationError::FatalError("output amount must be non-negative".to_string())
            })?,
            gas: quote.gas.into(),
            new_state: Box::new(quote.new_state),
        })
    }

    fn delta_transition(
        &mut self,
        delta: ProtocolStateDelta,
        _tokens: &HashMap<Bytes, Token>,
        _balances: &Balances,
    ) -> Result<(), TransitionError<String>> {
        if let Some(liquidity) = delta
            .updated_attributes
            .get("liquidity")
        {
            self.set_liquidity(liquidity.clone().into());
        }
        if let Some(sqrt_price) = delta
            .updated_attributes
            .get("sqrt_ratio")
        {
            self.set_sqrt_ratio(U256::from_big_endian(sqrt_price));
        }

        match self {
            Self::Base(p) => {
                if let Some(tick) = delta.updated_attributes.get("tick") {
                    p.set_active_tick(tick.clone().into());
                }
            }
            Self::Oracle(_) => {} // The exact tick is not required for oracle pools
        }

        let changed_ticks = ticks_from_attributes(
            delta
                .updated_attributes
                .into_iter()
                .chain(
                    delta
                        .deleted_attributes
                        .into_iter()
                        .map(|key| (key, Bytes::new())),
                ),
        )
        .map_err(TransitionError::DecodeError)?;

        for changed_tick in changed_ticks {
            self.set_tick(changed_tick)
                .map_err(TransitionError::DecodeError)?;
        }

        self.reinstantiate();

        Ok(())
    }

    fn clone_box(&self) -> Box<dyn ProtocolSim> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn eq(&self, other: &dyn ProtocolSim) -> bool {
        other
            .as_any()
            .downcast_ref::<EkuboState>()
            .is_some_and(|other_state| self == other_state)
    }

    fn get_limits(
        &self,
        sell_token: Address,
        _buy_token: Address,
    ) -> Result<(BigUint, BigUint), SimulationError> {
        // TODO Update once exact out is supported
        Ok((
            self.get_limit(U256::from_big_endian(sell_token.as_slice()))?
                .into(),
            BigUint::ZERO,
        ))
    }
}

#[cfg(test)]
mod tests {
    use evm_ekubo_sdk::{math::tick::MIN_SQRT_RATIO, quoting::base_pool::BasePoolState};
    use num_traits::Zero;

    use super::*;
    use crate::evm::protocol::ekubo::test_pool::*;

    #[test]
    fn test_delta_transition() {
        let mut pool = EkuboState::Base(BasePool::new(
            POOL_KEY,
            BasePoolState { sqrt_ratio: MIN_SQRT_RATIO, liquidity: 0, active_tick_index: None },
            vec![].into(),
            MIN_TICK,
        ));

        let delta = ProtocolStateDelta { updated_attributes: attributes(), ..Default::default() };

        pool.delta_transition(delta, &HashMap::default(), &Balances::default())
            .unwrap();

        assert_eq!(state(), pool);
    }

    #[test]
    // Compare against the reference implementation
    fn test_get_amount_out() {
        let state = state();

        let amount = 100_u8;

        let tycho_quote = state
            .get_amount_out(BigUint::from(amount), &token0(), &token1())
            .unwrap();

        let EkuboState::Base(pool) = state else {
            panic!();
        };

        let reference_quote = pool
            .quote(TokenAmount { token: POOL_KEY.token0, amount: amount.into() })
            .unwrap();

        let tycho_out: u64 = tycho_quote.amount.try_into().unwrap();
        let reference_out: u64 = reference_quote
            .calculated_amount
            .try_into()
            .unwrap();

        assert_eq!(tycho_out, reference_out);
    }

    #[test]
    fn test_get_limits() {
        let state = state();

        let max_amount_in = state
            .get_limits(
                Address::from_word(POOL_KEY.token0.to_big_endian().into()),
                Address::from_word(POOL_KEY.token1.to_big_endian().into()),
            )
            .unwrap()
            .0;

        assert!(!max_amount_in.is_zero());

        state
            .get_amount_out(max_amount_in, &token0(), &token1())
            .unwrap();
    }
}
