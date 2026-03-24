use alloy_primitives::{address, Address};
use alloy_sol_types::sol;

// ─── BNB Chain contract addresses ───────────────────────────────────────────

/// SyntheticSharesOracle on BNB Chain.
pub const SYNTHETIC_SHARES_ORACLE: Address =
    address!("F4Fd8a1B412633e10527454137A29Db7Aa35F15e");

/// Multicall3 — universal batching contract (same address on all EVM chains).
pub const MULTICALL3: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");

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

    /// Ondo SyntheticSharesOracle — official on-chain oracle for GM token sValue.
    #[sol(rpc)]
    interface ISyntheticSharesOracle {
        /// Returns (sharesPerToken, field2) for a single token.
        function assetData(address token) external view returns (
            uint256 sharesPerToken,
            uint256 field2
        );
    }

    /// Multicall3 — batch multiple calls into one RPC request.
    #[sol(rpc)]
    interface IMulticall3 {
        struct Call3 {
            address target;
            bool allowFailure;
            bytes callData;
        }

        struct Result {
            bool success;
            bytes returnData;
        }

        function aggregate3(Call3[] calldata calls) external payable returns (Result[] memory returnData);
    }
}
