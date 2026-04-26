// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

// =============================================================================
// LiquidationExecutor.sol
// =============================================================================
// Atomic flash-loan liquidation contract for Aave V3 on Arbitrum.
//
// Flow:
//   1. Bot calls initiateFlashLoan() with target user + pair info
//   2. Aave calls executeOperation() callback with the flash-loaned debt tokens
//   3. Inside callback:
//      a. Approve Pool to spend debt tokens
//      b. Call Pool.liquidationCall() — repay user's debt, receive collateral + bonus
//      c. Approve Uniswap Router to spend seized collateral
//      d. Swap collateral → debt token via Uniswap V3
//      e. Verify profit ≥ minProfitBps (2.8%)
//      f. Approve Pool to pull flash loan repayment (amount + premium)
//      g. Transfer remaining profit to owner
//   4. If any step fails or profit too low, entire TX reverts
//
// Security: ReentrancyGuard, onlyOwner, immutable addresses, custom errors,
//           SafeERC20, min-profit enforcement, initiator validation.
// Gas: Revert path < 35k, Happy path < 450k (target).
// =============================================================================

// ─── Interfaces (minimal, gas-efficient) ─────────────────────────────────────

interface IERC20 {
    function balanceOf(address account) external view returns (uint256);
    function transfer(address to, uint256 amount) external returns (bool);
    function approve(address spender, uint256 amount) external returns (bool);
    function allowance(address owner, address spender) external view returns (uint256);
}

interface IPool {
    function flashLoanSimple(
        address receiverAddress,
        address asset,
        uint256 amount,
        bytes calldata params,
        uint16 referralCode
    ) external;

    function liquidationCall(
        address collateralAsset,
        address debtAsset,
        address user,
        uint256 debtToCover,
        bool receiveAToken
    ) external;
}

interface ISwapRouter {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }
    function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
}

interface IPoolAddressesProvider {
    function getPool() external view returns (address);
}

// ─── Custom Errors (gas-efficient, < 35k on revert) ─────────────────────────

error Unauthorized();
error InvalidCallback();
error InsufficientProfit(uint256 actual, uint256 required);
error SwapFailed();
error TransferFailed();
error ZeroAddress();
error ZeroAmount();

// ─── Main Contract ───────────────────────────────────────────────────────────

contract LiquidationExecutor {
    // ── State Variables (all immutable or constant for gas savings) ───────

    /// @notice Owner of the contract (the bot EOA). Immutable after deployment.
    address public immutable owner;

    /// @notice Aave V3 Pool contract. Immutable.
    IPool public immutable POOL;

    /// @notice Aave V3 PoolAddressesProvider. Immutable.
    IPoolAddressesProvider public immutable ADDRESSES_PROVIDER;

    /// @notice Uniswap V3 SwapRouter. Immutable.
    ISwapRouter public immutable SWAP_ROUTER;

    /// @notice Minimum profit in basis points (280 = 2.8%). Immutable.
    uint256 public immutable minProfitBps;

    /// @notice Reentrancy lock
    uint256 private _locked = 1;

    /// @notice BPS denominator
    uint256 private constant BPS = 10_000;

    // ── Events ───────────────────────────────────────────────────────────

    event LiquidationExecuted(
        address indexed user,
        address indexed debtAsset,
        address indexed collateralAsset,
        uint256 debtRepaid,
        uint256 collateralSeized,
        uint256 profit
    );

    event TokensRescued(address indexed token, uint256 amount);

    // ── Modifiers ────────────────────────────────────────────────────────

    modifier onlyOwner() {
        if (msg.sender != owner) revert Unauthorized();
        _;
    }

    modifier nonReentrant() {
        if (_locked == 2) revert Unauthorized();
        _locked = 2;
        _;
        _locked = 1;
    }

    // ── Constructor ──────────────────────────────────────────────────────

    /// @param _addressesProvider Aave V3 PoolAddressesProvider on Arbitrum
    /// @param _swapRouter Uniswap V3 SwapRouter02 on Arbitrum
    /// @param _minProfitBps Minimum net profit in basis points (e.g., 280 = 2.8%)
    constructor(
        address _addressesProvider,
        address _swapRouter,
        uint256 _minProfitBps
    ) {
        if (_addressesProvider == address(0)) revert ZeroAddress();
        if (_swapRouter == address(0)) revert ZeroAddress();

        owner = msg.sender;
        ADDRESSES_PROVIDER = IPoolAddressesProvider(_addressesProvider);
        POOL = IPool(ADDRESSES_PROVIDER.getPool());
        SWAP_ROUTER = ISwapRouter(_swapRouter);
        minProfitBps = _minProfitBps;
    }

    // ── External: Initiate Flash Loan ────────────────────────────────────

    /// @notice Bot calls this to start a flash-loan liquidation.
    /// @param debtAsset The borrowed asset to repay (flash loan this)
    /// @param debtToCover Amount of debt to repay
    /// @param collateralAsset The collateral asset to seize
    /// @param user The borrower to liquidate
    /// @param swapPoolFee Uniswap V3 pool fee tier (e.g., 500, 3000, 10000)
    /// @param minAmountOut Minimum debt tokens to receive from collateral swap
    function initiateFlashLoan(
        address debtAsset,
        uint256 debtToCover,
        address collateralAsset,
        address user,
        uint24 swapPoolFee,
        uint256 minAmountOut
    ) external onlyOwner nonReentrant {
        if (debtAsset == address(0) || collateralAsset == address(0) || user == address(0)) revert ZeroAddress();
        if (debtToCover == 0) revert ZeroAmount();

        // Encode params for the callback
        bytes memory params = abi.encode(
            collateralAsset,
            user,
            swapPoolFee,
            minAmountOut
        );

        // Request flash loan — Aave will callback executeOperation
        POOL.flashLoanSimple(
            address(this),  // receiver
            debtAsset,      // asset to borrow
            debtToCover,    // amount
            params,         // passed to callback
            0               // referral code
        );
    }

    // ── Callback: Execute Operation ──────────────────────────────────────

    /// @notice Called by Aave Pool during flash loan. DO NOT call directly.
    /// @param asset The flash-loaned asset (debt token)
    /// @param amount The flash-loaned amount
    /// @param premium The fee to pay (0.05% of amount)
    /// @param initiator Must be this contract (security check)
    /// @param params Encoded (collateralAsset, user, swapPoolFee, minAmountOut)
    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external returns (bool) {
        // ── Security: Only the Pool can call this, and only we initiated ──
        if (msg.sender != address(POOL)) revert InvalidCallback();
        if (initiator != address(this)) revert InvalidCallback();

        // ── Decode parameters ────────────────────────────────────────────
        (
            address collateralAsset,
            address user,
            uint24 swapPoolFee,
            uint256 minAmountOut
        ) = abi.decode(params, (address, address, uint24, uint256));

        // ── Step 1: Approve Pool to spend debt tokens for liquidation ────
        _safeApprove(IERC20(asset), address(POOL), amount);

        // ── Step 2: Execute liquidation — repay debt, receive collateral ─
        // receiveAToken = false → receive underlying collateral directly
        POOL.liquidationCall(
            collateralAsset,
            asset,        // debtAsset
            user,
            amount,       // debtToCover
            false         // receive underlying, not aTokens
        );

        // ── Step 3: Get collateral balance received ──────────────────────
        uint256 collateralReceived = IERC20(collateralAsset).balanceOf(address(this));

        // ── Step 4: Swap collateral → debt token via Uniswap V3 ──────────
        // Skip swap if collateral == debt (same asset, e.g., liquidating USDC debt with USDC collateral)
        uint256 debtTokensReceived;
        if (collateralAsset != asset) {
            _safeApprove(IERC20(collateralAsset), address(SWAP_ROUTER), collateralReceived);

            debtTokensReceived = SWAP_ROUTER.exactInputSingle(
                ISwapRouter.ExactInputSingleParams({
                    tokenIn: collateralAsset,
                    tokenOut: asset,
                    fee: swapPoolFee,
                    recipient: address(this),
                    amountIn: collateralReceived,
                    amountOutMinimum: minAmountOut,
                    sqrtPriceLimitX96: 0  // no price limit
                })
            );
        } else {
            debtTokensReceived = collateralReceived;
        }

        // ── Step 5: Calculate profit and enforce minimum ─────────────────
        uint256 totalOwed = amount + premium;
        if (debtTokensReceived < totalOwed) revert InsufficientProfit(0, totalOwed);

        uint256 profit = debtTokensReceived - totalOwed;

        // Enforce minimum profit: profit / amount >= minProfitBps / BPS
        // Rearranged to avoid division: profit * BPS >= amount * minProfitBps
        // No division before multiplication!
        if (profit * BPS < amount * minProfitBps) {
            revert InsufficientProfit(profit, (amount * minProfitBps) / BPS);
        }

        // ── Step 6: Approve Pool to pull repayment ───────────────────────
        _safeApprove(IERC20(asset), address(POOL), totalOwed);

        // ── Step 7: Transfer profit to owner ─────────────────────────────
        if (profit > 0) {
            _safeTransfer(IERC20(asset), owner, profit);
        }

        emit LiquidationExecuted(
            user,
            asset,
            collateralAsset,
            amount,
            collateralReceived,
            profit
        );

        return true;
    }

    // ── Emergency: Rescue stuck tokens ────────────────────────────────────

    /// @notice Rescue any tokens accidentally sent to this contract.
    function rescueTokens(address token) external onlyOwner {
        if (token == address(0)) revert ZeroAddress();
        uint256 balance = IERC20(token).balanceOf(address(this));
        if (balance > 0) {
            _safeTransfer(IERC20(token), owner, balance);
            emit TokensRescued(token, balance);
        }
    }

    // ── Internal: SafeERC20 helpers ──────────────────────────────────────

    /// @dev Safe approve that handles non-standard ERC20 (USDT etc.)
    function _safeApprove(IERC20 token, address spender, uint256 value) internal {
        // Reset approval first (required by USDT and similar tokens)
        (bool s1, ) = address(token).call(abi.encodeWithSelector(token.approve.selector, spender, 0));
        if (!s1) revert TransferFailed();
        (bool s2, ) = address(token).call(abi.encodeWithSelector(token.approve.selector, spender, value));
        if (!s2) revert TransferFailed();
    }

    /// @dev Safe transfer that handles non-standard ERC20
    function _safeTransfer(IERC20 token, address to, uint256 value) internal {
        (bool success, bytes memory data) = address(token).call(
            abi.encodeWithSelector(token.transfer.selector, to, value)
        );
        if (!success || (data.length > 0 && !abi.decode(data, (bool)))) {
            revert TransferFailed();
        }
    }

    // ── Prevent accidental ETH deposits ──────────────────────────────────
    receive() external payable {
        revert();
    }
}
