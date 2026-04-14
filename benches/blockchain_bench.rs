use criterion::{Criterion, criterion_group, criterion_main};

use curs3d::core::chain::{Blockchain, GenesisConfig};
use curs3d::core::transaction::Transaction;
use curs3d::crypto::dilithium::KeyPair;
use curs3d::crypto::hash;

fn bench_keygen(c: &mut Criterion) {
    c.bench_function("dilithium5_keygen", |b| {
        b.iter(KeyPair::generate);
    });
}

fn bench_tx_signing(c: &mut Criterion) {
    let kp = KeyPair::generate();
    let kp2 = KeyPair::generate();
    let to_address = hash::address_bytes_from_public_key(&kp2.public_key);

    c.bench_function("tx_sign_transfer", |b| {
        b.iter(|| {
            let mut tx = Transaction::new(
                "bench-chain",
                kp.public_key.clone(),
                to_address.clone(),
                1_000_000,
                1_000,
                0,
            );
            tx.sign(&kp);
            tx
        });
    });
}

fn bench_tx_verification(c: &mut Criterion) {
    let kp = KeyPair::generate();
    let kp2 = KeyPair::generate();
    let to_address = hash::address_bytes_from_public_key(&kp2.public_key);

    let mut tx = Transaction::new(
        "bench-chain",
        kp.public_key.clone(),
        to_address,
        1_000_000,
        1_000,
        0,
    );
    tx.sign(&kp);

    c.bench_function("tx_verify_signature", |b| {
        b.iter(|| tx.verify_signature());
    });
}

fn bench_sha3_hash(c: &mut Criterion) {
    let data = vec![42u8; 1024];
    c.bench_function("sha3_hash_1kb", |b| {
        b.iter(|| hash::sha3_hash(&data));
    });
}

fn bench_merkle_root(c: &mut Criterion) {
    let leaves: Vec<Vec<u8>> = (0u64..1000)
        .map(|i| hash::sha3_hash(&i.to_le_bytes()))
        .collect();

    c.bench_function("merkle_root_1000_leaves", |b| {
        b.iter(|| hash::merkle_root(&leaves));
    });
}

fn bench_merkle_proof(c: &mut Criterion) {
    let leaves: Vec<Vec<u8>> = (0u64..1000)
        .map(|i| hash::sha3_hash(&i.to_le_bytes()))
        .collect();

    c.bench_function("merkle_proof_gen_1000_leaves", |b| {
        b.iter(|| hash::merkle_proof(&leaves, 500));
    });
}

fn bench_merkle_verify(c: &mut Criterion) {
    let leaves: Vec<Vec<u8>> = (0u64..1000)
        .map(|i| hash::sha3_hash(&i.to_le_bytes()))
        .collect();
    let root = hash::merkle_root(&leaves);
    let proof = hash::merkle_proof(&leaves, 500);

    c.bench_function("merkle_proof_verify_1000_leaves", |b| {
        b.iter(|| hash::verify_merkle_proof(&leaves[500], &proof, 500, &root));
    });
}

fn bench_block_creation(c: &mut Criterion) {
    let validator_kp = KeyPair::generate();
    let validator_addr = hash::address_bytes_from_public_key(&validator_kp.public_key);
    let faucet_kp = KeyPair::generate();

    let config = GenesisConfig {
        chain_id: "bench-chain".to_string(),
        chain_name: "Benchmark Chain".to_string(),
        allocations: vec![
            curs3d::core::chain::GenesisAllocation {
                public_key: validator_kp.public_key_hex(),
                balance: 10_000_000_000,
                staked_balance: 1_000_000_000,
            },
            curs3d::core::chain::GenesisAllocation {
                public_key: faucet_kp.public_key_hex(),
                balance: 100_000_000_000,
                staked_balance: 0,
            },
        ],
        ..GenesisConfig::default()
    };

    c.bench_function("block_creation_10tx", |b| {
        b.iter_batched(
            || {
                let mut chain = Blockchain::from_genesis(config.clone()).unwrap();
                // Add some pending transactions
                for i in 0..10u64 {
                    let mut tx = Transaction::new(
                        "bench-chain",
                        faucet_kp.public_key.clone(),
                        validator_addr.clone(),
                        1_000,
                        1_000,
                        i,
                    );
                    tx.sign(&faucet_kp);
                    let _ = chain.add_transaction(tx);
                }
                chain
            },
            |chain| {
                let _ = chain.create_block(&validator_kp);
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_state_root(c: &mut Criterion) {
    let validator_kp = KeyPair::generate();

    let config = GenesisConfig {
        chain_id: "bench-chain".to_string(),
        chain_name: "Benchmark Chain".to_string(),
        allocations: vec![curs3d::core::chain::GenesisAllocation {
            public_key: validator_kp.public_key_hex(),
            balance: 10_000_000_000,
            staked_balance: 1_000_000_000,
        }],
        ..GenesisConfig::default()
    };

    let chain = Blockchain::from_genesis(config).unwrap();

    c.bench_function("state_root_computation", |b| {
        b.iter(|| Blockchain::compute_state_root(&chain.accounts));
    });
}

criterion_group!(
    benches,
    bench_keygen,
    bench_tx_signing,
    bench_tx_verification,
    bench_sha3_hash,
    bench_merkle_root,
    bench_merkle_proof,
    bench_merkle_verify,
    bench_block_creation,
    bench_state_root,
);
criterion_main!(benches);
