// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {ERC20Permit} from "@openzeppelin/contracts/token/ERC20/extensions/ERC20Permit.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";

/// @title Curs3dToken
/// @notice Capped, pausable, role-mintable ERC-20 with EIP-2612 `permit` support.
/// @dev Trust model:
///      - `owner()` (Ownable) controls minter allow-list and pause/unpause.
///      - Minters can mint up to `cap()`. Anyone can `burn` their own tokens.
///      - `cap` is fixed at construction and immutable thereafter.
///      Audit notes:
///      - All transfer/mint/burn paths use the OZ ERC20 `_update` hook so the
///        `whenNotPaused` gate applies uniformly (including `permit`-spent
///        transfers).
///      - Custom errors used throughout for gas savings + introspection.
contract Curs3dToken is ERC20, ERC20Permit, Ownable, Pausable {
    /// @notice Thrown when a non-minter tries to mint.
    error NotMinter();
    /// @notice Thrown when minting would exceed `cap`.
    error CapExceeded();
    /// @notice Thrown when constructor receives a zero or invalid cap.
    error InvalidCap();
    /// @notice Thrown when an admin function receives the zero address.
    error ZeroAddress();

    /// @notice Maximum total supply. Set once at construction.
    uint256 public immutable cap;

    /// @notice Addresses authorised to call `mint`. The owner is always implicitly authorised.
    mapping(address => bool) public minters;

    /// @notice Emitted when `setMinter` toggles an address.
    event MinterUpdated(address indexed minter, bool allowed);

    /// @param initialSupply  Tokens minted to `initialOwner` at deploy.
    /// @param maxSupply      Hard cap. Must be > 0 and >= `initialSupply`.
    /// @param initialOwner   Receives `initialSupply` and becomes `owner()`.
    constructor(uint256 initialSupply, uint256 maxSupply, address initialOwner)
        ERC20("CURS3D Portfolio Token", "CURS3D")
        ERC20Permit("CURS3D Portfolio Token")
        Ownable(initialOwner)
    {
        if (maxSupply == 0 || initialSupply > maxSupply) {
            revert InvalidCap();
        }
        if (initialOwner == address(0)) revert ZeroAddress();
        cap = maxSupply;
        if (initialSupply != 0) _mint(initialOwner, initialSupply);
    }

    /// @notice Toggle a minter. Owner only.
    /// @param minter   Address to grant or revoke.
    /// @param allowed  Grant if true, revoke if false.
    function setMinter(address minter, bool allowed) external onlyOwner {
        if (minter == address(0)) revert ZeroAddress();
        minters[minter] = allowed;
        emit MinterUpdated(minter, allowed);
    }

    /// @notice Mint new tokens up to the cap. Caller must be the owner or an allow-listed minter.
    /// @param to     Recipient.
    /// @param amount Amount in base units.
    function mint(address to, uint256 amount) external whenNotPaused {
        if (msg.sender != owner() && !minters[msg.sender]) revert NotMinter();
        if (totalSupply() + amount > cap) revert CapExceeded();
        _mint(to, amount);
    }

    /// @notice Burn tokens from the caller's balance.
    /// @param amount Amount in base units.
    function burn(uint256 amount) external {
        _burn(msg.sender, amount);
    }

    /// @notice Pause all transfers, mints, and burns. Owner only. Emergency lever.
    function pause() external onlyOwner {
        _pause();
    }

    /// @notice Resume normal operation.
    function unpause() external onlyOwner {
        _unpause();
    }

    /// @dev OZ v5 `_update` hook — gates every balance change through `whenNotPaused`.
    function _update(address from, address to, uint256 value) internal override whenNotPaused {
        super._update(from, to, value);
    }
}
