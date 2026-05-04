// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Curs3dToken} from "./Curs3dToken.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import {SafeERC20} from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";
import {ReentrancyGuard} from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

/// @title Curs3dStaking
/// @notice Single-asset staking with linear time-based rewards.
/// @dev Trust model:
///      - `owner()` can pause and tune `rewardRatePerSecond` (capped by `MAX_REWARD_RATE`).
///      - Rewards are minted on demand → the staking contract must be a minter on the token.
///      Math: pending = stakedBalance * rewardRatePerSecond * elapsedSeconds / 1e18.
///      Therefore `rewardRatePerSecond` is in "wad per second per staked wei" — a value of
///      `0.001 ether` (1e15) means 0.1 % of stake accrues per second.
contract Curs3dStaking is Ownable, Pausable, ReentrancyGuard {
    using SafeERC20 for IERC20;

    /// @notice Hard ceiling on the reward rate to bound mint inflation.
    /// @dev 1 wad/second per staked wei = 100% per second. We cap at 1e16 (1%/sec) which
    ///      is already extreme but leaves room for promo periods.
    uint256 public constant MAX_REWARD_RATE = 1e16;
    /// @notice Math denominator for the reward formula (1 wad).
    uint256 private constant WAD = 1 ether;

    /// @notice Thrown on zero-amount stake/unstake.
    error ZeroAmount();
    /// @notice Thrown when unstaking more than the caller's stake.
    error NotEnoughStaked();
    /// @notice Thrown when admin sets a reward rate above `MAX_REWARD_RATE`.
    error RewardRateTooHigh();
    /// @notice Thrown when constructor receives the zero address.
    error ZeroAddress();

    /// @notice The staked token. Also the reward token (rewards are freshly minted).
    Curs3dToken public immutable token;
    /// @notice Reward rate, see contract NatSpec.
    uint256 public rewardRatePerSecond;
    /// @notice Sum of `stakedBalance[*]`. Used by invariant tests.
    uint256 public totalStaked;

    /// @notice Per-account staked balance.
    mapping(address => uint256) public stakedBalance;
    /// @notice Per-account accrued (but unclaimed) rewards.
    mapping(address => uint256) public rewardDebt;
    /// @notice Last checkpoint timestamp per account.
    mapping(address => uint256) public lastUpdate;

    /// @notice Emitted on a successful stake.
    event Staked(address indexed account, uint256 amount);
    /// @notice Emitted on a successful unstake.
    event Unstaked(address indexed account, uint256 amount);
    /// @notice Emitted on a successful reward claim.
    event RewardsClaimed(address indexed account, uint256 amount);
    /// @notice Emitted when admin updates the reward rate.
    event RewardRateUpdated(uint256 rewardRatePerSecond);

    /// @param token_                 Staked + reward token. Must allow this contract as a minter.
    /// @param rewardRatePerSecond_   Initial rate (see math note in contract NatSpec).
    /// @param owner_                 Initial owner.
    constructor(Curs3dToken token_, uint256 rewardRatePerSecond_, address owner_) Ownable(owner_) {
        if (address(token_) == address(0) || owner_ == address(0)) revert ZeroAddress();
        if (rewardRatePerSecond_ > MAX_REWARD_RATE) revert RewardRateTooHigh();
        token = token_;
        rewardRatePerSecond = rewardRatePerSecond_;
        emit RewardRateUpdated(rewardRatePerSecond_);
    }

    /// @notice Compute pending (unclaimed) rewards for `account`.
    function pendingRewards(address account) public view returns (uint256) {
        uint256 staked = stakedBalance[account];
        if (staked == 0) return rewardDebt[account];
        uint256 elapsed = block.timestamp - lastUpdate[account];
        return rewardDebt[account] + ((staked * rewardRatePerSecond * elapsed) / WAD);
    }

    /// @notice Stake `amount` of token. Caller must have approved `amount` to this contract.
    /// @dev CEI: state mutated, then `safeTransferFrom`. `nonReentrant` for defence in depth.
    function stake(uint256 amount) external whenNotPaused nonReentrant {
        if (amount == 0) revert ZeroAmount();
        _checkpoint(msg.sender);
        stakedBalance[msg.sender] += amount;
        totalStaked += amount;
        IERC20(address(token)).safeTransferFrom(msg.sender, address(this), amount);
        emit Staked(msg.sender, amount);
    }

    /// @notice Withdraw `amount` from the caller's stake. Pending rewards are checkpointed.
    /// @dev CEI: state mutated before transfer.
    function unstake(uint256 amount) external nonReentrant {
        if (amount == 0) revert ZeroAmount();
        if (stakedBalance[msg.sender] < amount) revert NotEnoughStaked();
        _checkpoint(msg.sender);
        stakedBalance[msg.sender] -= amount;
        totalStaked -= amount;
        IERC20(address(token)).safeTransfer(msg.sender, amount);
        emit Unstaked(msg.sender, amount);
    }

    /// @notice Claim accrued rewards. Mints them fresh from the token contract.
    /// @return reward Amount minted.
    function claimRewards() external whenNotPaused nonReentrant returns (uint256 reward) {
        _checkpoint(msg.sender);
        reward = rewardDebt[msg.sender];
        if (reward == 0) return 0;
        // Effects before interactions.
        rewardDebt[msg.sender] = 0;
        token.mint(msg.sender, reward);
        emit RewardsClaimed(msg.sender, reward);
    }

    /// @notice Update the reward rate. Owner only. Bounded by `MAX_REWARD_RATE`.
    function setRewardRate(uint256 rewardRatePerSecond_) external onlyOwner {
        if (rewardRatePerSecond_ > MAX_REWARD_RATE) revert RewardRateTooHigh();
        rewardRatePerSecond = rewardRatePerSecond_;
        emit RewardRateUpdated(rewardRatePerSecond_);
    }

    /// @notice Pause `stake` and `claimRewards`. `unstake` remains open so users can always exit.
    function pause() external onlyOwner {
        _pause();
    }

    /// @notice Unpause.
    function unpause() external onlyOwner {
        _unpause();
    }

    function _checkpoint(address account) internal {
        rewardDebt[account] = pendingRewards(account);
        lastUpdate[account] = block.timestamp;
    }
}
