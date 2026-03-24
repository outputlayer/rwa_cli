use alloy_primitives::{address, Address};
use alloy_sol_types::sol;

// ─── BNB Chain contract addresses ───────────────────────────────────────────

/// Ondo GM TokenManager on BNB Chain.
pub const GM_TOKEN_MANAGER: Address = address!("91f8Aff3738825e8eB16FC6f6b1A7A4647bDB299");

/// USDon (Ondo U.S. Dollar Token) on BNB Chain.
pub const USDON: Address = address!("1f8955E640Cbd9abc3C3Bb408c9E2E1f5F20DfE6");

/// SyntheticSharesOracle on BNB Chain.
pub const SYNTHETIC_SHARES_ORACLE: Address =
    address!("F4Fd8a1B412633e10527454137A29Db7Aa35F15e");

/// GMTokenLimitOrder on BNB Chain.
pub const GM_LIMIT_ORDER: Address = address!("96b525B1a93f31E65F4aAf18C53842eD28525D48");

/// OndoIDRegistry on BNB Chain.
pub const ONDO_ID_REGISTRY: Address = address!("898128f9f22c0192da0c5acd394d9eeac461d911");

// ─── Solidity interfaces (ABI bindings) ─────────────────────────────────────

sol! {
    /// Standard ERC-20 interface (read-only subset).
    #[sol(rpc)]
    interface IERC20 {
        function name() external view returns (string);
        function symbol() external view returns (string);
        function decimals() external view returns (uint8);
        function totalSupply() external view returns (uint256);
        function balanceOf(address account) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
    }

    /// Ondo SyntheticSharesOracle — returns per-token oracle data.
    /// On-chain verified: selector 0x41fee44a returns (uint256, uint256).
    #[sol(rpc)]
    interface ISyntheticSharesOracle {
        function assetData(address token) external view returns (
            uint256 sharesPerToken,
            uint256 field2
        );
    }

    /// Ondo GMTokenManager — subscribe (mint) and redeem.
    #[sol(rpc)]
    interface IGMTokenManager {
        function subscribe(address token, uint256 amount, uint256 minTokensOut) external;
        function executionId() external view returns (uint256);
        function ondoIDRegistry() external view returns (address);
    }

    /// Chainlink AggregatorV3Interface — standard price feed interface.
    #[sol(rpc)]
    interface IAggregatorV3 {
        function latestRoundData() external view returns (
            uint80 roundId,
            int256 answer,
            uint256 startedAt,
            uint256 updatedAt,
            uint80 answeredInRound
        );
        function decimals() external view returns (uint8);
        function description() external view returns (string);
    }

    /// Ondo GMTokenLimitOrder — limit order functions.
    #[sol(rpc)]
    interface IGMLimitOrder {
        function createBuyOrderExactIn(
            address token,
            address paymentToken,
            uint256 amountIn,
            uint256 minAmountOut,
            uint256 expiry
        ) external;
        function createSellOrderExactIn(
            address token,
            address paymentToken,
            uint256 amountIn,
            uint256 minAmountOut,
            uint256 expiry
        ) external;
        function cancelOrder(uint256 orderId) external;
        function isOrderActive(uint256 orderId) external view returns (bool);
    }
}
