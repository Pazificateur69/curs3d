# Solidity Portfolio Brief

## One-Line Pitch

`A Solidity portfolio built around CURS3D: ERC20-style token, faucet, staking, governance, attestations, vault, escrow, Foundry tests, deployment script, and browser dApp.`

## Interview Pitch

`I started by building an experimental Rust L1, then added a focused Solidity portfolio around it. The Solidity side shows practical EVM work: token mechanics, faucet UX, staking rewards, token-weighted governance, on-chain attestations, a simplified vault, an escrow contract, Foundry unit tests, fuzz tests, invariant tests, deployment scripting, and a testnet dApp.`

## What To Show A Recruiter

1. Open `contracts/README.md`.
2. Run `forge test --summary`.
3. Show `src/Curs3dToken.sol`, `src/Curs3dStaking.sol`, and `src/Curs3dGovernance.sol`.
4. Show fuzz and invariant tests in `test/FuzzAndInvariant.t.sol`.
5. Open `dapp/index.html` or serve it with `make serve-dapp`.
6. Show testnet deployment addresses once deployed.

## Strong Points

- Self-contained contracts that are easy to review.
- Foundry test coverage across normal flows and failure flows.
- Fuzzing and invariant testing included.
- dApp is simple but end-to-end.
- The project connects Solidity work to a larger blockchain engineering story.

## Honest Limitations

- Not audited.
- Not production-ready.
- Some contracts are intentionally simplified to stay portfolio-sized.
- Governance does not use snapshots.
- Vault is ERC4626-style, not complete ERC4626.

## Next Upgrades

- Add OpenZeppelin version for production comparison.
- Add Slither CI.
- Add contract verification after testnet deployment.
- Add a hosted demo page with real deployed addresses.
- Add a short article explaining the threat model and tradeoffs.
