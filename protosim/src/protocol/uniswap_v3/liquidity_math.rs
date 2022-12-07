// Solidity spec: function addDelta(uint128 x, int128 y) internal pure returns (uint128 z) {
fn add_liquidity_delta(x: u128, y: i128) -> u128 {
    if y < 0 {
        x - (-y as u128)
    } else {
        x + (y as u128)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_liquidity_delta() {
        // TODO: check more cases. e.g. overflowing 128 bits
        let x = 10000;
        let y = -1000;

        let res = add_liquidity_delta(x, y);

        assert_eq!(res, 9000);
    }
}
