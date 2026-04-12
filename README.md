# CURS3D

**Quantum-Resistant Blockchain — Built in Rust**

CURS3D is a Layer 1 blockchain designed to be secure against quantum computers. It uses post-quantum cryptographic algorithms standardized by NIST.

## Features

- **CRYSTALS-Dilithium** (Level 5) — Post-quantum digital signatures
- **SHA-3** (Keccak-256) — Quantum-resistant hashing with Merkle trees
- **Proof of Stake** — Energy-efficient consensus with slashing
- **P2P Networking** — libp2p with Gossipsub and mDNS discovery
- **CLI Node** — Full node with wallet management

## Quick Start

```bash
# Build
cargo build --release

# Create a wallet
./target/release/curs3d wallet --output my_wallet.json

# Run a node
./target/release/curs3d node --port 4337

# Check status
./target/release/curs3d status
```

## Architecture

```
src/
├── crypto/          # Dilithium signatures + SHA-3 hashing
├── core/            # Blocks, transactions, blockchain state
├── consensus/       # Proof of Stake with validator selection
├── network/         # P2P via libp2p (Gossipsub + mDNS)
├── wallet/          # Key generation and management
└── main.rs          # CLI interface
```

## Tech Stack

| Component | Technology |
|-----------|-----------|
| Language | Rust |
| Signatures | CRYSTALS-Dilithium 5 |
| Hashing | SHA-3 (Keccak-256) |
| Consensus | Proof of Stake |
| Networking | libp2p + Gossipsub |
| Discovery | mDNS |

## License

MIT
