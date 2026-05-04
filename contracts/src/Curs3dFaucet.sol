// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Curs3dToken} from "./Curs3dToken.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

/// @title Curs3dFaucet
/// @notice Drip faucet that mints Curs3dToken to claimers, gated by a per-address cooldown.
/// @dev Trust model:
///      - `owner()` can pause/unpause and tune `claimAmount` / `cooldown` within bounds.
///      - The faucet must be granted minter rights on the token (`token.setMinter(faucet, true)`).
///      Per-IP rate-limiting is intentionally NOT enforced on-chain (impossible — the chain
///      never sees the IP). It belongs to the off-chain reverse proxy / captcha layer.
contract Curs3dFaucet is Ownable, Pausable, ReentrancyGuard {
    /// @notice Hard ceiling on `claimAmount`, enforced by `setConfig`.
    uint256 public constant MAX_CLAIM_AMOUNT = 10_000 ether;
    /// @notice Hard ceiling on `cooldown`, enforced by `setConfig`. 30 days protects against lock-out.
    uint256 public constant MAX_COOLDOWN = 30 days;

    /// @notice Thrown when a claim arrives before the per-address cooldown elapses.
    error CooldownActive(uint256 nextClaimAt);
    /// @notice Thrown when admin sets `claimAmount` above `MAX_CLAIM_AMOUNT`.
    error AmountTooHigh();
    /// @notice Thrown when admin sets `cooldown` above `MAX_COOLDOWN`.
    error CooldownTooHigh();
    /// @notice Thrown when constructor receives the zero address.
    error ZeroAddress();
    /// @notice Thrown when `claimAmount` is set to zero.
    error ZeroAmount();

    /// @notice The token this faucet drips. Must allow this faucet as a minter.
    Curs3dToken public immutable token;
    /// @notice Tokens dispensed per `claim()` call.
    uint256 public claimAmount;
    /// @notice Minimum interval between claims for a given address.
    uint256 public cooldown;
    /// @notice Last successful claim timestamp per address.
    mapping(address => uint256) public lastClaimAt;

    /// @notice Emitted on every successful claim.
    event Claimed(address indexed account, uint256 amount, uint256 nextClaimAt);
    /// @notice Emitted when `setConfig` mutates parameters.
    event FaucetConfigUpdated(uint256 claimAmount, uint256 cooldown);

    /// @param token_       The drippable Curs3dToken.
    /// @param claimAmount_ Initial drip size.
    /// @param cooldown_    Initial per-address cooldown.
    /// @param owner_       Initial owner.
    constructor(Curs3dToken token_, uint256 claimAmount_, uint256 cooldown_, address owner_)
        Ownable(owner_)
    {
        if (address(token_) == address(0) || owner_ == address(0)) revert ZeroAddress();
        if (claimAmount_ == 0) revert ZeroAmount();
        if (claimAmount_ > MAX_CLAIM_AMOUNT) revert AmountTooHigh();
        if (cooldown_ > MAX_COOLDOWN) revert CooldownTooHigh();
        token = token_;
        claimAmount = claimAmount_;
        cooldown = cooldown_;
        emit FaucetConfigUpdated(claimAmount_, cooldown_);
    }

    /// @notice Drip `claimAmount` tokens to `msg.sender`, subject to cooldown.
    /// @dev CEI: state updated before the external `mint` call. `nonReentrant` guards
    ///      against malicious tokens (defence in depth — this token is non-callback).
    function claim() external whenNotPaused nonReentrant {
        uint256 last = lastClaimAt[msg.sender];
        uint256 nextClaimAt = last + cooldown;
        if (last != 0 && block.timestamp < nextClaimAt) revert CooldownActive(nextClaimAt);

        // Effects before interactions.
        lastClaimAt[msg.sender] = block.timestamp;
        uint256 amount = claimAmount;

        // Interactions.
        token.mint(msg.sender, amount);

        emit Claimed(msg.sender, amount, block.timestamp + cooldown);
    }

    /// @notice Update faucet parameters. Owner only.
    function setConfig(uint256 claimAmount_, uint256 cooldown_) external onlyOwner {
        if (claimAmount_ == 0) revert ZeroAmount();
        if (claimAmount_ > MAX_CLAIM_AMOUNT) revert AmountTooHigh();
        if (cooldown_ > MAX_COOLDOWN) revert CooldownTooHigh();
        claimAmount = claimAmount_;
        cooldown = cooldown_;
        emit FaucetConfigUpdated(claimAmount_, cooldown_);
    }

    /// @notice Pause `claim()` (e.g. during incidents). Owner only.
    function pause() external onlyOwner {
        _pause();
    }

    /// @notice Resume `claim()`.
    function unpause() external onlyOwner {
        _unpause();
    }
}
