pub mod base;
pub mod oracle;

use evm_ekubo_sdk::{math::uint::U256, quoting::types::NodeKey};

use super::state::EkuboState;

#[enum_delegate::register]
pub trait EkuboPool {
    fn key(&self) -> &NodeKey;

    fn sqrt_ratio(&self) -> U256;

    fn set_sqrt_ratio(&mut self, sqrt_ratio: U256);
    fn set_liquidity(&mut self, liquidity: u128);

    fn reinstantiate(&mut self);
}

pub struct EkuboPoolQuote {
    pub calculated_amount: i128,
    pub gas: u64,
    pub new_state: EkuboState,
}
