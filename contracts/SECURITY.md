# Security Notes

This folder is a portfolio project, not production infrastructure. The goal is to show Solidity engineering discipline, clear threat awareness, and testable contracts.

## Security Posture

- No external audit has been performed.
- Contracts are designed for testnets and portfolio review.
- Do not use these contracts to custody real funds.
- Deployment keys must be testnet-only.
- `.env`, broadcast logs, and generated deployment JSON files are ignored.

## Main Design Risks

### Token

`Curs3dToken` is ERC20-style and self-contained. It intentionally avoids OpenZeppelin imports so a reviewer can inspect the full implementation in one repo. For production, use audited OpenZeppelin primitives unless there is a strong reason not to.

Known tradeoffs:
- owner can authorize minters
- no permit
- no pausing
- no role timelock

### Faucet

`Curs3dFaucet` uses per-wallet cooldown only. It does not prevent sybil claims. That is acceptable for a testnet faucet, not for real token distribution.

### Staking

`Curs3dStaking` mints rewards directly through a minter role. It is intentionally simple.

Known tradeoffs:
- reward rate is owner controlled
- no reward pool accounting
- no historical reward schedule

### Governance

`Curs3dGovernance` uses current token balances as voting weight. This is simple, but not production governance because voters can move tokens around unless snapshotting or delegation is added.

Known tradeoffs:
- no snapshot votes
- no timelock
- no executable proposal payloads
- no delegation

### Attestations

`Curs3dAttestations` stores hashes and URIs. It should not store private data directly on-chain.

Known tradeoffs:
- issuers are owner-managed
- URI permanence depends on the storage layer
- revocation does not delete historical data

### Vault

`Curs3dVault` is ERC4626-style, not a full ERC4626 implementation.

Known tradeoffs:
- no allowance for share withdrawal by third parties
- simplified share math
- no yield strategy

### Escrow

`DigitalEscrow` forwards ETH directly to the seller.

Known tradeoffs:
- no dispute process
- no platform fee
- no ERC721/ERC1155 transfer
- simple ETH-only settlement

## Recommended Production Upgrades

- Replace self-contained primitives with OpenZeppelin where appropriate.
- Add access-control timelocks.
- Add governance snapshots and delegation.
- Add pausing for emergency paths.
- Add reentrancy protection for complex payment flows.
- Add event indexing review.
- Run Slither and Echidna.
- Run an external audit before handling real value.
