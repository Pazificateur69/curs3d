# CURS3D Solidity Portfolio

This folder turns `CURS3D` into a Solidity-focused portfolio project. It is separate from the Rust L1 codebase and is meant to show practical smart contract engineering: ERC20 mechanics, faucet UX, staking, governance, attestations, mini DeFi primitives, tests, and testnet deployment workflow.

## What I Built

- `Curs3dToken`: capped ERC20-style portfolio token with owner-controlled minters.
- `Curs3dFaucet`: testnet faucet with per-wallet cooldown.
- `Curs3dStaking`: simple staking contract with linear rewards minted by an authorized minter.
- `Curs3dGovernance`: simple token-weighted proposal/voting/execution flow.
- `Curs3dAttestations`: issuer-based registry for certificates, credentials, or durable attestations.
- `Curs3dVault`: simplified ERC4626-style vault project.
- `DigitalEscrow`: small digital asset marketplace/escrow project.
- `dapp/`: static browser dApp for wallet connect, faucet mint, attestation issue/read, and transaction display.

## Stack

- Solidity `0.8.26`
- Foundry `forge`
- OpenZeppelin Contracts (ERC20, ERC20Permit, Ownable, Pausable, ReentrancyGuard, SafeERC20)
- Vanilla HTML/CSS/JavaScript dApp
- Browser wallet via `window.ethereum`
- **Primary deploy target: CURS3D's own EVM** (the L1's `revm 38` integration, RPC at https://rpc.curs3d.fr/eth, chain ID `1800329576`)
- Also deployable to Sepolia / Base Sepolia for portability demos

## Why This Matters For A Solidity Role

The Rust L1 proves low-level blockchain interest. This folder proves Solidity execution:
- contract design
- access control
- custom errors
- ERC20 allowance flows
- testnet UX
- fuzz testing
- invariant testing
- deployment scripting
- clear recruiter-facing documentation

## Contracts

| Contract | Purpose |
| --- | --- |
| `src/Curs3dToken.sol` | capped ERC20-style token |
| `src/Curs3dFaucet.sol` | testnet token mint faucet |
| `src/Curs3dStaking.sol` | stake token and claim linear rewards |
| `src/Curs3dGovernance.sol` | proposal, vote, quorum and execution |
| `src/Curs3dAttestations.sol` | issue and revoke durable attestations |
| `src/portfolio/Curs3dVault.sol` | simplified ERC4626-style vault |
| `src/portfolio/DigitalEscrow.sol` | digital listing escrow |

## Tests

Run:

```bash
forge test -vv
```

Current coverage style:
- unit tests for token, faucet, staking, governance, attestations, vault, escrow
- access-control tests for owner-only functions
- revert tests for expected failures
- fuzz tests for token transfer and cap behavior
- invariant test that token supply never exceeds cap

## Build

```bash
forge build
```

## Common Commands

```bash
make fmt
make build
make test
make dry-run
make serve-dapp
```

## Deploy To CURS3D Testnet (primary target)

CURS3D's L1 ships a revm 38 EVM accessible at `https://rpc.curs3d.fr/eth` (chain ID `1800329576` / `0x6b4ed968`). The deploy script is chain-agnostic — it writes `deployments/<chainId>.json` automatically.

```bash
# 1. Generate a fresh EVM key (or reuse an existing testnet keystore).
cast wallet new-mnemonic
cast wallet import curs3d-deployer --interactive

# 2. Fund the deployer address with native CUR. Easiest is to top up from the
#    native faucet wallet on node1 (operator action, see deploy/DEPLOY_RUNBOOK.md
#    section "Send CUR to an address"). 10 000 CUR is plenty for the 7-contract
#    deploy + a few hundred test transactions.

# 3. Deploy.
FORCE=true forge script script/DeployPortfolio.s.sol:DeployPortfolio \
  --rpc-url "${CURS3D_RPC_URL:-https://rpc.curs3d.fr/eth}" \
  --keystore keystores/<keystore-file> \
  --password <keystore-password> \
  --sender <deployer-address> \
  --broadcast \
  --slow \
  --legacy
# FORCE=true overwrites deployments/1800329576.json on intentional redeploys.
# --slow makes Foundry wait tx-by-tx, which is friendlier to a small testnet.
# --legacy avoids extra fee-history negotiation during portfolio demos.
```

Output: `deployments/1800329576.json` with all 7 contract addresses.

## Deploy To Sepolia

Create a local `.env` from `.env.example`, then run:

```bash
forge script script/DeployPortfolio.s.sol:DeployPortfolio \
  --rpc-url "$SEPOLIA_RPC_URL" \
  --broadcast
```

## Deploy To Base Sepolia

```bash
forge script script/DeployPortfolio.s.sol:DeployPortfolio \
  --rpc-url "$BASE_SEPOLIA_RPC_URL" \
  --broadcast
```

The script writes the latest deployment addresses to:

```text
deployments/latest.json
```

Copy that JSON into the dApp's `Deployment JSON` field and press `Load JSON`.

## Mini dApp

Open:

```text
dapp/index.html
```

The dApp can:
- connect a wallet
- switch to Sepolia or Base Sepolia
- call the faucet
- issue an attestation
- read an attestation by id
- approve and stake tokens
- claim staking rewards
- create, vote, and execute governance proposals
- list and buy escrow items
- show recent transaction hashes

Serve locally:

```bash
make serve-dapp
```

Then open:

```text
http://127.0.0.1:8088
```

## Known Risks And Limitations

- These contracts are portfolio/demo contracts, not production contracts.
- The token uses OpenZeppelin's `ERC20`/`ERC20Permit`/`Ownable`/`Pausable` directly (audited libraries), with custom mint/burn/cap logic on top.
- Governance uses current token balances clamped by a lazy per-voter snapshot at first vote — not chain-wide historical snapshots. Defends against "transfer to friend, vote again" but not multi-address whales.
- Staking reward logic is intentionally simple and does not model a production emissions schedule.
- The vault is a simplified ERC4626-style example, not a full ERC4626 implementation.
- The escrow forwards ETH directly to the seller and is suitable as a small demo, not a marketplace backend.
- No external audit has been performed.

## Recruiter Summary

`I built a Solidity portfolio inside my CURS3D blockchain project: a capped ERC20-style token, faucet, staking, governance, attestation registry, vault, escrow, Foundry tests with fuzzing/invariants, deployment scripts, and a browser dApp for testnet interaction.`

## Extra Review Docs

- `SECURITY.md`: threat model, risks, and production upgrade path.
- `PORTFOLIO.md`: interview pitch and recruiter-facing demo flow.
- `RUNBOOK_FR.md`: instructions en français pour lancer, déployer et utiliser la dApp.
