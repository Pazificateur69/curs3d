<p align="center">
  <br>
  <strong style="font-size: 2rem;">CURS3D</strong><br>
  <em>Quantum-Resistant Layer 1 Blockchain</em>
  <br><br>
  <a href="https://github.com/Pazificateur69/curs3d/actions"><img src="https://github.com/Pazificateur69/curs3d/workflows/CI/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-2024_edition-orange.svg" alt="Rust 2024"></a>
  <img src="https://img.shields.io/badge/tests-cargo%20test-brightgreen.svg" alt="cargo test">
  <img src="https://img.shields.io/badge/clippy-0%20warnings-brightgreen.svg" alt="0 clippy warnings">
  <img src="https://img.shields.io/badge/quantum-resistant-blueviolet.svg" alt="Quantum Resistant">
  <br>
  <a href="https://curs3d.fr">Website</a> · <a href="https://curs3d.fr/docs.html">Docs</a> · <a href="https://curs3d.fr/whitepaper.html">Whitepaper</a> · <a href="https://curs3d.fr/explorer.html">Explorer</a> · <a href="https://curs3d.fr/examples.html">Tutorials</a>
</p>

---

CURS3D is a **Layer 1 blockchain written from scratch in Rust**, designed to resist quantum computing attacks. It uses NIST-standardized post-quantum cryptography (CRYSTALS-Dilithium 5), BFT Proof of Stake consensus with explicit 2/3 finality, a WASM smart contract engine with instruction-level gas metering, and an EIP-1559 dynamic fee market. Every component is original — no fork of Ethereum, Cosmos, or Substrate.

## Why CURS3D?

**The quantum threat is real.** NIST finalized post-quantum cryptography standards in 2024. Most blockchains still rely on ECDSA/EdDSA, which will be broken by Shor's algorithm. CURS3D is built from the ground up with quantum-resistant primitives — not retrofitted.

| What | How |
|------|-----|
| **Signatures** | CRYSTALS-Dilithium Level 5 (NIST FIPS 204) |
| **Hashing** | SHA-3 Keccak-256, double-hash blocks, Merkle trees |
| **Wallet encryption** | AES-256-GCM + Argon2 KDF |
| **Consensus** | BFT Proof of Stake, 2/3 stake-weighted finality |
| **Smart contracts** | WASM VM (Wasmer 5 + Cranelift), per-instruction fuel metering |
| **Fee market** | EIP-1559 dynamic base fee, priority fees, gas refunds |
| **Fork choice** | Heaviest chain by cumulative proposer stake |
| **Slashing** | Cryptographic equivocation proof, 33% penalty, 64-block jail |
| **Networking** | libp2p 0.54 (Gossipsub + mDNS + noise + yamux) |
| **Storage** | sled embedded DB, schema v4, auto-migration |

## Quick Start

```bash
# Build from source
git clone https://github.com/Pazificateur69/curs3d.git
cd curs3d
cargo build --release

# Create password files for non-interactive deploys
printf '%s\n' 'change-this-validator-password' > validator.password

# Create an encrypted wallet (CRYSTALS-Dilithium 5 keypair)
./target/release/curs3d wallet --output validator.json --password-file validator.password

# Generate a real public testnet genesis from the wallet you will operate
./target/release/curs3d genesis \
  --output genesis.public-testnet.json \
  --validator-wallet validator.json \
  --validator-password-file validator.password

# Publish a stable bootnode address for your VPS
./target/release/curs3d bootnode-address \
  --data-dir curs3d_data \
  --public-addr /dns4/node.example.com/tcp/4337

# Run a validator node
./target/release/curs3d node \
  --validator-wallet validator.json \
  --validator-password-file validator.password \
  --genesis-config genesis.public-testnet.json \
  --public-addr /dns4/node.example.com/tcp/4337

# The node exposes:
#   P2P:      0.0.0.0:4337  (Gossipsub + mDNS)
#   HTTP API: 127.0.0.1:8080
#   TCP RPC:  127.0.0.1:9545

# Check chain status
curl http://localhost:8080/api/status | jq .data
```

### With Docker

```bash
docker compose up -d           # Start 2-node network
curl localhost:8080/api/status  # Query the chain
docker compose down             # Stop
```

### Public VPS

Use the deployment assets in [`deploy/`](deploy/):

- [`deploy/DEPLOY_VPS.md`](deploy/DEPLOY_VPS.md)
- [`deploy/docker-compose.public.yml`](deploy/docker-compose.public.yml)
- [`deploy/systemd/curs3d.service`](deploy/systemd/curs3d.service)
- [`deploy/nginx/curs3d.conf`](deploy/nginx/curs3d.conf)

## What's Built (Current State)

CURS3D is an **advanced L1 prototype** — not yet mainnet-ready, but technically substantial. Here's what exists in the codebase today, all tested:

- **BFT PoS consensus** with epoch-frozen validator sets and 2/3 finality threshold
- **WASM smart contracts** with Wasmer 5, Cranelift backend, 11 host functions, instruction-level fuel metering
- **EIP-1559 fee market** with dynamic base fee, separate max/priority fees, gas refunds, mempool pressure management
- **12 transaction types**: native transfers/staking, WASM contracts, CUR-20 token ops, governance
- **Fork choice tree** with heaviest-chain rule, automatic reorg, finality boundary, non-canonical pruning
- **Provable slashing** with cryptographic EquivocationEvidence (dual Dilithium signatures)
- **State sync** with Merkle-verified snapshot chunks, manifest protocol, finalized checkpoints
- **Account + storage proofs** exportable via API (Merkle inclusion proofs)
- **Encrypted wallets** (AES-256-GCM + Argon2), auto-migration from legacy format
- **Protocol versioning** with upgrade-at-height activation and network topic filtering
- **Persistent storage** (sled, 10 trees, schema v4 with auto-migration)
- **REST API** (11 endpoints) + TCP RPC + CLI
- **Block explorer** web UI with live dashboard
- **Docker** multi-stage build + docker-compose
- **CI/CD** pipeline (check, test, clippy 0 warnings, fmt)
- **Repository test suite** exercised through `cargo test`

### What Remains for Mainnet

- External security audit (consensus, VM, crypto)
- Peer scoring + anti-spam hardening
- State trie (MPT or Verkle tree)
- Indexed receipts + log filters
- Prometheus metrics + structured logging
- Epoch-based rewards + inactivity penalties
- Fuzzing + long-run soak tests
- Contract SDK (Rust + AssemblyScript)

## Architecture

```
src/
  api/             HTTP REST API (hyper 1.x, 128 max connections, 1MB body limit)
  consensus/       BFT PoS, FinalityVote, FinalityTracker, EquivocationEvidence, slashing
  core/
    block.rs         BlockHeader, Block, genesis, signatures, verification
    blocktree.rs     BlockTree, fork choice (heaviest chain), pruning
    chain.rs         Blockchain state, validation, reorg, fee market, snapshots
    transaction.rs   6 types: Transfer, Stake, Unstake, Coinbase, DeployContract, CallContract
    receipt.rs       Execution receipts with gas details and logs
    state_proof.rs   AccountProof, StorageProof (Merkle inclusion)
  crypto/
    dilithium.rs     CRYSTALS-Dilithium Level 5 (pqcrypto)
    hash.rs          SHA-3, double-hash, Merkle trees/proofs, address derivation
  network/         libp2p P2P (Gossipsub + mDNS), sync, block production, state sync
  rpc/             TCP JSON RPC (port 9545)
  storage/         sled DB (10 trees, schema v4, snapshots, migration)
  vm/
    mod.rs           Wasmer WASM execution, host functions, fuel middleware
    gas.rs           Gas cost schedule
    state.rs         ContractState (code, storage, owner)
  wallet/          Encrypted wallet (AES-256-GCM + Argon2)
  main.rs          CLI entry point (clap 4)

website/           Documentation site (6 pages)
```

## REST API

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/status` | Chain height, finalized height, epoch, validators, protocol version |
| GET | `/api/block/:height` | Block with transactions, state root, merkle root |
| GET | `/api/blocks?from=&limit=` | Paginated blocks (max 100) |
| GET | `/api/account/:address` | Balance, nonce, staked balance |
| GET | `/api/account/:address/proof` | Merkle account proof |
| GET | `/api/contract/:addr/storage/:key/proof` | Merkle storage proof |
| GET | `/api/tx/:hash` | Transaction by hash |
| GET | `/api/pending` | Mempool pending transactions |
| GET | `/api/validators` | Active validator set with stakes |
| POST | `/api/tx/submit` | Submit signed transaction (auth optional) |
| POST | `/api/tx/estimate` | Dry-run: gas estimate, fees, replacement check |

All responses: `{"ok": true, "data": {...}}` or `{"ok": false, "error": "..."}`.

Auth: set `CURS3D_API_TOKEN` env var to require `Authorization: Bearer <token>` on POST endpoints.

## Smart Contracts

CURS3D runs WebAssembly contracts via Wasmer 5 with Cranelift. The VM injects fuel metering per instruction — contracts with unmetered loops are rejected at deploy time.

| Operation | Gas Cost |
|-----------|----------|
| Base transaction | 21,000 |
| Contract deploy | 32,000 |
| Contract call | 2,600 |
| Storage read | 200 |
| Storage write | 5,000 |
| Log emit | 375 |
| Per byte (data) | 16 |
| WASM loop tick | 50 |

**Host functions:** `storage_get`, `storage_set`, `storage_read`, `storage_write_bytes`, `emit_log`, `emit_log_bytes`, `input`, `input_len`, `input_read`, `consume_gas`, `loop_tick`

## Consensus

1. **Validator Selection** — Deterministic, stake-weighted using `SHA-3(height || prev_hash)`
2. **Block Production** — Selected validator signs blocks every 10 seconds
3. **Finality Votes** — Validators sign attestations (`block_hash || height || epoch`)
4. **Finalization** — Block irreversible when votes representing >= 2/3 total stake are collected
5. **Slashing** — Dual-signed block headers at same height = cryptographic proof. 33% stake penalty + jail
6. **Epochs** — Validator set frozen per epoch (default 32 blocks). No mid-epoch manipulation
7. **Fork Choice** — Heaviest cumulative proposer-stake wins. Finality boundary prevents deep reorgs

## Testing

```bash
cargo test
cargo clippy         # 0 warnings (CI enforces -D warnings)
cargo fmt --check    # Enforced formatting
```

Coverage: cryptographic operations, block validation, transaction flow (all 6 types), staking/unstaking, slashing with evidence, BFT finality threshold, fork choice, block tree pruning, wallet encryption/decryption, storage persistence, WASM VM execution, gas metering, state sync snapshots, epoch management.

## Genesis Configuration

```json
{
  "chain_id": "my-testnet",
  "chain_name": "My Testnet",
  "block_reward": 50000000,
  "minimum_stake": 1000000000,
  "unstake_delay_blocks": 10,
  "epoch_length": 32,
  "jail_duration_blocks": 64,
  "block_gas_limit": 10000000,
  "initial_base_fee_per_gas": 0,
  "base_fee_change_denominator": 8,
  "allocations": [
    {
      "public_key": "0xVALIDATOR_PUBLIC_KEY_HEX",
      "balance": 1000000000000,
      "staked_balance": 5000000000
    }
  ],
  "upgrades": [
    { "height": 1000, "version": 2, "description": "Enable feature X" }
  ]
}
```

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-feature`)
3. Make your changes
4. Ensure `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` all pass
5. Open a Pull Request

All contributions welcome — whether it's code, documentation, bug reports, or ideas.

## License

MIT License. See [LICENSE](LICENSE) for details.
