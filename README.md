<p align="center">
  <strong>CURS3D</strong><br>
  Quantum-Resistant Layer 1 Blockchain
</p>

<p align="center">
  <a href="https://github.com/Pazificateur69/curs3d/actions"><img src="https://github.com/Pazificateur69/curs3d/workflows/CI/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="MIT License"></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/rust-1.94+-orange.svg" alt="Rust 1.94+"></a>
  <img src="https://img.shields.io/badge/tests-46%20passing-brightgreen.svg" alt="46 tests">
  <img src="https://img.shields.io/badge/quantum-resistant-blueviolet.svg" alt="Quantum Resistant">
</p>

---

CURS3D is a **Layer 1 blockchain** written from scratch in Rust, designed to remain secure against quantum computing attacks. It uses NIST-standardized post-quantum cryptography, BFT Proof of Stake consensus with explicit finality, and ships with a full REST API, block explorer, and Docker support.

## Highlights

- **Post-Quantum Cryptography** -- CRYSTALS-Dilithium Level 5 signatures + SHA-3 hashing
- **BFT Finality** -- Blocks finalized when 2/3+ of stake votes (irreversible)
- **Fork Choice** -- Heaviest-chain rule with automatic reorg and finality boundary
- **Provable Slashing** -- Equivocation detection with cryptographic proof, 33% stake penalty
- **REST API** -- 10 HTTP endpoints with CORS for browser/app integration
- **Block Explorer** -- Web UI with live dashboard, account lookup, faucet, tx search
- **Stake/Unstake** -- Lock and unlock tokens for validation
- **Encrypted Wallets** -- AES-256-GCM + Argon2 password-based encryption
- **Docker Ready** -- Multi-stage build + docker-compose for 2-node networks
- **CI Pipeline** -- GitHub Actions with check, test, clippy, fmt

## Quick Start

```bash
# Clone and build
git clone https://github.com/Pazificateur69/curs3d.git
cd curs3d
cargo build --release

# Create an encrypted wallet
./target/release/curs3d wallet --output my_wallet.json

# Run a validator node
./target/release/curs3d node --validator-wallet my_wallet.json

# Check blockchain status
./target/release/curs3d status
```

### With Docker

```bash
docker compose up -d          # Start 2-node network
curl localhost:8080/api/status # Check via REST API
docker compose down            # Stop
```

## CLI Reference

```
curs3d node     [--port 4337] [--data-dir curs3d_data] [--validator-wallet path]
                [--bootnode /ip4/.../tcp/4337] [--rpc-addr 127.0.0.1:9545]
                [--genesis-config genesis.json]

curs3d wallet   [--output wallet.json]          # Create encrypted wallet
curs3d info     [--wallet wallet.json]           # Show wallet details
curs3d send     --to CUR... --amount 100         # Send tokens
                [--wallet wallet.json] [--fee 1000]
curs3d stake    --amount 1000                    # Stake tokens for validation
                [--wallet wallet.json] [--fee 1000]
curs3d status   [--data-dir curs3d_data]         # Blockchain status
                [--rpc-addr 127.0.0.1:9545]
```

## REST API

The node exposes an HTTP API on port **8080** (alongside the TCP RPC on 9545).

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/status` | Chain height, finalized height, validators, pending txs |
| GET | `/api/block/:height` | Block details with transactions |
| GET | `/api/blocks?from=&limit=` | Paginated recent blocks |
| GET | `/api/account/:address` | Balance, nonce, staked balance |
| GET | `/api/tx/:hash` | Transaction lookup |
| GET | `/api/pending` | Pending transactions |
| GET | `/api/validators` | Active validators with stakes |
| GET | `/api/faucet/:address` | Testnet faucet (100 CUR) |
| POST | `/api/tx/submit` | Submit a signed transaction |
| OPTIONS | `*` | CORS preflight |

```bash
# Examples
curl http://localhost:8080/api/status
curl http://localhost:8080/api/block/0
curl http://localhost:8080/api/validators
curl http://localhost:8080/api/faucet/YOUR_ADDRESS_HEX
```

## Architecture

```
src/
  api/           HTTP REST API server (hyper)
  consensus/     BFT PoS, finality votes, equivocation evidence, slashing
  core/
    block.rs       Block headers, signatures, verification
    blocktree.rs   Fork choice tree, heaviest-chain rule, pruning
    chain.rs       Blockchain state, validation, reorg, genesis config
    transaction.rs Transfer, Stake, Unstake, Coinbase
  crypto/
    dilithium.rs   CRYSTALS-Dilithium Level 5 (NIST PQC)
    hash.rs        SHA-3, double-hash, Merkle trees, address derivation
  network/       libp2p P2P with Gossipsub, mDNS, sync protocol
  rpc/           TCP JSON RPC for CLI
  storage/       sled persistent storage (blocks, accounts, evidence, pending)
  wallet/        Encrypted wallet (AES-256-GCM + Argon2)
  main.rs        CLI entry point (clap)

website/
  index.html       Landing page
  docs.html        Full documentation
  examples.html    Step-by-step tutorials (9 tutorials)
  whitepaper.html  Technical whitepaper v2.0
  explorer.html    Block explorer (connects to REST API)
  style.css        Shared styles
  script.js        Shared scripts
```

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Language | Rust 2024 edition |
| Signatures | CRYSTALS-Dilithium Level 5 (pqcrypto) |
| Hashing | SHA-3 Keccak-256 (sha3) |
| Wallet Encryption | AES-256-GCM + Argon2 |
| Consensus | BFT Proof of Stake (2/3 finality) |
| Fork Choice | Heaviest chain by cumulative stake |
| P2P Network | libp2p (Gossipsub + mDNS) |
| HTTP API | hyper 1.x |
| Storage | sled embedded database |
| Serialization | bincode + serde_json |
| CLI | clap 4 |
| Containers | Docker multi-stage build |
| CI | GitHub Actions |

## Consensus

CURS3D uses **BFT Proof of Stake** with explicit finality:

1. **Validator Selection** -- Deterministic, stake-weighted selection using SHA-3 hash of (block_height + prev_hash)
2. **Block Production** -- Selected validator produces and signs blocks every 10 seconds
3. **Finality Votes** -- Validators sign attestations for blocks they accept
4. **Finalization** -- When votes representing >= 2/3 of total staked amount are collected, the block becomes irreversible
5. **Slashing** -- Validators caught signing two blocks at the same height lose 33% of stake and are jailed
6. **Fork Choice** -- If competing chains exist, the one with higher cumulative proposer-stake wins

## Transactions

| Type | Description |
|------|-------------|
| **Transfer** | Send CUR tokens to an address |
| **Stake** | Lock tokens to become a validator |
| **Unstake** | Unlock staked tokens back to available balance |
| **Coinbase** | Block reward (automatically created per block) |

## Testing

```bash
cargo test          # Run all 46 tests
cargo clippy        # Lint check
cargo fmt --check   # Format check
```

Test coverage includes: cryptographic operations, block validation, transaction flow, staking/unstaking, slashing with evidence, BFT finality threshold, fork choice, block tree pruning, wallet encryption/decryption, storage persistence.

## Genesis Configuration

Create a `genesis.json` to customize the chain:

```json
{
  "chain_name": "my-testnet",
  "block_reward": 50000000,
  "minimum_stake": 1000000000,
  "allocations": [
    {
      "public_key": "0xVALIDATOR_PUBLIC_KEY_HEX",
      "balance": 1000000000000,
      "staked_balance": 5000000000
    }
  ]
}
```

```bash
curs3d node --genesis-config genesis.json --validator-wallet validator.json
```

## Website

The project includes a complete documentation website:

- **Landing Page** -- Project overview with animated design
- **Documentation** -- CLI reference, architecture, API docs, consensus details
- **Examples** -- 9 step-by-step tutorials
- **Whitepaper** -- Technical whitepaper v2.0 (13 sections)
- **Block Explorer** -- Live blockchain dashboard with faucet

Serve locally: `cd website && python3 -m http.server 3000`

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/amazing`)
3. Commit your changes
4. Push to the branch
5. Open a Pull Request

Please ensure `cargo test`, `cargo clippy`, and `cargo fmt --check` pass before submitting.

## License

MIT License. See [LICENSE](LICENSE) for details.
