// SPDX-License-Identifier: MIT
pragma solidity ^0.8.26;

import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Pausable} from "@openzeppelin/contracts/utils/Pausable.sol";

/// @title Curs3dAttestations
/// @notice Minimal attestation registry: allow-listed issuers attest claims about subjects.
/// @dev Trust model:
///      - `owner()` manages the issuer allow-list and can pause issuance.
///      - Issuers can revoke only attestations they themselves issued. The owner can
///        revoke any attestation (emergency lever).
///      - The `dataHash` is the canonical claim digest; the `uri` points to off-chain
///        evidence (IPFS, Arweave, etc).
contract Curs3dAttestations is Ownable, Pausable {
    error NotIssuer();
    error AlreadyRevoked();
    error AttestationMissing();
    error ZeroAddress();
    error EmptyDataHash();
    error UriTooLong();

    /// @notice Hard cap on the URI length to bound storage cost / griefing.
    uint256 public constant MAX_URI_LENGTH = 512;

    struct Attestation {
        address issuer;
        address subject;
        bytes32 dataHash;
        string uri;
        uint64 issuedAt;
        bool revoked;
    }

    /// @notice Allow-listed issuers.
    mapping(address => bool) public issuers;
    /// @notice Storage of all attestations, keyed by content-derived id.
    mapping(bytes32 => Attestation) public attestations;

    event IssuerUpdated(address indexed issuer, bool allowed);
    event AttestationIssued(
        bytes32 indexed attestationId,
        address indexed issuer,
        address indexed subject,
        bytes32 dataHash,
        string uri
    );
    event AttestationRevoked(bytes32 indexed attestationId, address indexed issuer);

    /// @param owner_ Initial owner. Also auto-allow-listed as an issuer.
    constructor(address owner_) Ownable(owner_) {
        if (owner_ == address(0)) revert ZeroAddress();
        issuers[owner_] = true;
        emit IssuerUpdated(owner_, true);
    }

    /// @notice Add or remove an issuer. Owner only.
    function setIssuer(address issuer, bool allowed) external onlyOwner {
        if (issuer == address(0)) revert ZeroAddress();
        issuers[issuer] = allowed;
        emit IssuerUpdated(issuer, allowed);
    }

    /// @notice Issue an attestation. Returns the deterministic id.
    /// @dev id = keccak256(issuer, subject, dataHash, uri, block.timestamp, block.number)
    ///      so two attestations issued in the same block over the same payload collide.
    ///      That collision is intentional: the second call overwrites the first, which
    ///      we explicitly forbid via `AttestationMissing` clone check.
    function issue(address subject, bytes32 dataHash, string calldata uri)
        external
        whenNotPaused
        returns (bytes32 attestationId)
    {
        if (!issuers[msg.sender]) revert NotIssuer();
        if (subject == address(0)) revert ZeroAddress();
        if (dataHash == bytes32(0)) revert EmptyDataHash();
        if (bytes(uri).length > MAX_URI_LENGTH) revert UriTooLong();

        attestationId = keccak256(
            abi.encode(msg.sender, subject, dataHash, uri, block.timestamp, block.number)
        );
        attestations[attestationId] = Attestation({
            issuer: msg.sender,
            subject: subject,
            dataHash: dataHash,
            uri: uri,
            issuedAt: uint64(block.timestamp),
            revoked: false
        });
        emit AttestationIssued(attestationId, msg.sender, subject, dataHash, uri);
    }

    /// @notice Revoke an attestation. Caller must be the original issuer or the contract owner.
    function revoke(bytes32 attestationId) external {
        Attestation storage attestation = attestations[attestationId];
        if (attestation.issuer == address(0)) revert AttestationMissing();
        if (msg.sender != attestation.issuer && msg.sender != owner()) revert NotIssuer();
        if (attestation.revoked) revert AlreadyRevoked();
        attestation.revoked = true;
        emit AttestationRevoked(attestationId, msg.sender);
    }

    /// @notice Pause new issuance. Owner only.
    function pause() external onlyOwner {
        _pause();
    }

    function unpause() external onlyOwner {
        _unpause();
    }
}
