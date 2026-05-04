// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

/// @title Curs3dVault
/// @notice Minimal ERC-4626-style share vault wrapping a single ERC-20 asset.
/// @dev Trust model:
///      - `owner()` can pause user-facing entry points only. Withdrawals stay open at all
///        times so users can always exit (no censorship risk).
///      - Inflation attack mitigation: a small amount of "virtual shares + virtual assets"
///        is added to the conversion math (OZ pattern). This neutralises the well-known
///        first-depositor donation attack on empty vaults.
///      - Strictly non-rebasing assets only. Fee-on-transfer or rebasing tokens will
///        break the share accounting.
contract Curs3dVault is Ownable, Pausable, ReentrancyGuard {
    using SafeERC20 for IERC20;

    /// @notice Inflation-attack mitigation virtual offset (10 decimals offset, OZ default).
    uint256 private constant DECIMALS_OFFSET = 10 ** 10;

    error ZeroAmount();
    error InsufficientShares();
    error NotShareOwner();
    error ZeroAddress();

    /// @notice Wrapped asset.
    IERC20 public immutable asset;
    /// @dev Standard ERC-20 metadata (lower-case is the canonical convention).
    string public constant name = "CURS3D Portfolio Vault Share";
    string public constant symbol = "cvCURS3D";
    uint8 public constant decimals = 18;
    /// @notice Total share supply.
    uint256 public totalSupply;
    /// @notice Per-account share balance.
    mapping(address => uint256) public balanceOf;

    event Deposit(address indexed caller, address indexed owner, uint256 assets, uint256 shares);
    event Withdraw(
        address indexed caller,
        address indexed receiver,
        address indexed owner,
        uint256 assets,
        uint256 shares
    );

    /// @param asset_ Wrapped asset token.
    /// @param owner_ Initial owner.
    constructor(IERC20 asset_, address owner_) Ownable(owner_) {
        if (address(asset_) == address(0) || owner_ == address(0)) revert ZeroAddress();
        asset = asset_;
    }

    /// @notice Total assets currently held by the vault.
    function totalAssets() public view returns (uint256) {
        return asset.balanceOf(address(this));
    }

    /// @notice Convert an asset amount to shares using the inflation-protected formula.
    function convertToShares(uint256 assets) public view returns (uint256) {
        return (assets * (totalSupply + DECIMALS_OFFSET)) / (totalAssets() + 1);
    }

    /// @notice Convert a share amount to assets using the inflation-protected formula.
    function convertToAssets(uint256 shares) public view returns (uint256) {
        return (shares * (totalAssets() + 1)) / (totalSupply + DECIMALS_OFFSET);
    }

    /// @notice Deposit `assets` and mint shares to `receiver`.
    /// @dev CEI: shares are minted before the asset transfer-in. SafeERC20 enforces strict
    ///      check-and-revert on the transfer.
    function deposit(uint256 assets, address receiver)
        external
        whenNotPaused
        nonReentrant
        returns (uint256 shares)
    {
        if (assets == 0) revert ZeroAmount();
        if (receiver == address(0)) revert ZeroAddress();
        shares = convertToShares(assets);
        if (shares == 0) revert ZeroAmount();

        // Effects.
        totalSupply += shares;
        balanceOf[receiver] += shares;

        // Interactions.
        asset.safeTransferFrom(msg.sender, address(this), assets);
        emit Deposit(msg.sender, receiver, assets, shares);
    }

    /// @notice Burn shares from `owner` and send `assets` to `receiver`.
    /// @dev `unstake`-like semantics: caller must own the shares (no allowance flow yet —
    ///      this is intentional for portfolio simplicity; ERC-4626 allowance can be added).
    function withdraw(uint256 assets, address receiver, address owner)
        external
        nonReentrant
        returns (uint256 shares)
    {
        if (assets == 0) revert ZeroAmount();
        if (receiver == address(0) || owner == address(0)) revert ZeroAddress();
        if (msg.sender != owner) revert NotShareOwner();

        shares = convertToShares(assets);
        if (balanceOf[owner] < shares) revert InsufficientShares();

        // Effects.
        balanceOf[owner] -= shares;
        totalSupply -= shares;

        // Interactions.
        asset.safeTransfer(receiver, assets);
        emit Withdraw(msg.sender, receiver, owner, assets, shares);
    }

    /// @notice Pause new deposits. Withdrawals always remain open.
    function pause() external onlyOwner {
        _pause();
    }

    function unpause() external onlyOwner {
        _unpause();
    }
}
