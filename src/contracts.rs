// =============================================================================
// src/contracts.rs — Alloy sol! bindings for on-chain contract interactions
// =============================================================================
// Uses Alloy's sol! macro to generate type-safe Rust bindings from Solidity
// interface definitions. These are used for all RPC calls and TX encoding.

use alloy::sol;

// ─── Aave V3 Pool ────────────────────────────────────────────────────────────
sol! {
    #[sol(rpc)]
    interface IPool {
        /// Returns the list of all initialized reserve addresses
        function getReservesList() external view returns (address[] memory);

        /// Returns the user account data across all reserves
        function getUserAccountData(address user) external view returns (
            uint256 totalCollateralBase,
            uint256 totalDebtBase,
            uint256 availableBorrowsBase,
            uint256 currentLiquidationThreshold,
            uint256 ltv,
            uint256 healthFactor
        );

        /// Returns the user configuration bitmap
        function getUserConfiguration(address user) external view returns (uint256 data);

        /// Execute a flash loan (simple, single asset)
        function flashLoanSimple(
            address receiverAddress,
            address asset,
            uint256 amount,
            bytes calldata params,
            uint16 referralCode
        ) external;

        /// Liquidate an undercollateralized position
        function liquidationCall(
            address collateralAsset,
            address debtAsset,
            address user,
            uint256 debtToCover,
            bool receiveAToken
        ) external;
    }
}

// ─── Aave V3 Pool Data Provider ──────────────────────────────────────────────
sol! {
    #[sol(rpc)]
    interface IPoolDataProvider {
        /// Get reserve data (rates, indices, etc.)
        function getReserveData(address asset) external view returns (
            uint256 unbacked,
            uint256 accruedToTreasuryScaled,
            uint256 totalAToken,
            uint256 totalStableDebt,
            uint256 totalVariableDebt,
            uint256 liquidityRate,
            uint256 variableBorrowRate,
            uint256 stableBorrowRate,
            uint256 averageStableBorrowRate,
            uint256 liquidityIndex,
            uint256 variableBorrowIndex,
            uint40 lastUpdateTimestamp
        );

        /// Get user reserve data (balances, debt, etc.)
        function getUserReserveData(address asset, address user) external view returns (
            uint256 currentATokenBalance,
            uint256 currentStableDebt,
            uint256 currentVariableDebt,
            uint256 principalStableDebt,
            uint256 scaledVariableDebt,
            uint256 stableBorrowRate,
            uint256 liquidityRate,
            uint40 stableRateLastUpdated,
            bool usageAsCollateralEnabled
        );

        /// Get reserve configuration data (LTV, thresholds, bonuses)
        function getReserveConfigurationData(address asset) external view returns (
            uint256 decimals,
            uint256 ltv,
            uint256 liquidationThreshold,
            uint256 liquidationBonus,
            uint256 reserveFactor,
            bool usageAsCollateralEnabled,
            bool borrowingEnabled,
            bool stableBorrowRateEnabled,
            bool isActive,
            bool isFrozen
        );

        /// Get all reserves tokens (aToken, stableDebt, variableDebt addresses)
        function getAllReservesTokens() external view returns (TokenData[] memory);
    }

    /// Token name + address pair returned by getAllReservesTokens
    struct TokenData {
        string symbol;
        address tokenAddress;
    }
}

// ─── Aave V3 Pool Addresses Provider ─────────────────────────────────────────
sol! {
    #[sol(rpc)]
    interface IPoolAddressesProvider {
        function getPool() external view returns (address);
        function getPoolDataProvider() external view returns (address);
        function getPriceOracle() external view returns (address);
    }
}

// ─── Aave V3 Price Oracle ────────────────────────────────────────────────────
sol! {
    #[sol(rpc)]
    interface IAaveOracle {
        /// Get asset price in base currency (USD, 8 decimals)
        function getAssetPrice(address asset) external view returns (uint256);

        /// Get prices for multiple assets
        function getAssetsPrices(address[] calldata assets) external view returns (uint256[] memory);

        /// Get the base currency unit (1e8 for USD)
        function BASE_CURRENCY_UNIT() external view returns (uint256);
    }
}

// ─── Our LiquidationExecutor contract ────────────────────────────────────────
sol! {
    #[sol(rpc)]
    interface ILiquidationExecutor {
        function initiateFlashLoan(
            address debtAsset,
            uint256 debtToCover,
            address collateralAsset,
            address user,
            uint24 swapPoolFee,
            uint256 minAmountOut
        ) external;

        function owner() external view returns (address);
        function rescueTokens(address token) external;
    }
}
