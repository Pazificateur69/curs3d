use super::*;
use crate::crypto::dilithium::KeyPair;

#[test]
fn test_new_blockchain() {
    let chain = Blockchain::new();
    assert_eq!(chain.height(), 0);
    assert!(chain.is_valid());
}

#[test]
fn test_custom_genesis_activates_validator() {
    let validator = KeyPair::generate();
    let chain = Blockchain::from_genesis(GenesisConfig {
        chain_id: "curs3d-test".to_string(),
        chain_name: "curs3d-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 100,
            staked_balance: 5_000,
        }],
        ..Default::default()
    })
    .unwrap();

    assert_eq!(chain.active_validator_count(), 1);
    assert_eq!(chain.genesis_config.chain_name, "curs3d-test");
}

#[test]
fn test_create_and_add_block() {
    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();
    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();
    assert_eq!(chain.height(), 1);
    assert!(chain.is_valid());
}

#[test]
fn test_base_fee_rises_after_busy_block() {
    let validator = KeyPair::generate();
    let mut chain = Blockchain::from_genesis(GenesisConfig {
        chain_id: "base-fee-test".to_string(),
        chain_name: "base-fee-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        block_gas_limit: 100_000,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    })
    .unwrap();

    let wasm_code = br#"(module
            (memory (export "memory") 1)
            (func (export "curs3d_call"))
        )"#
    .to_vec();
    let mut deploy_tx = Transaction::deploy_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        wasm_code,
        80_000,
        0,
        0,
    )
    .with_fee_caps(1_000, 100);
    deploy_tx.sign(&validator);
    chain.add_transaction(deploy_tx).unwrap();

    // Genesis base_fee=0 for backwards compat, transitions to >=1 on first block
    let initial_fee = chain.current_base_fee_per_gas();
    assert_eq!(initial_fee, 0); // Genesis starts at 0
    let block1 = chain.create_block(&validator).unwrap();
    assert!(block1.header.gas_used > chain.target_block_gas_usage());
    chain.add_block(block1).unwrap();

    let block2 = chain.create_block(&validator).unwrap();
    assert!(block2.header.base_fee_per_gas > initial_fee);
}

#[test]
fn test_transaction_flow() {
    let mut chain = Blockchain::new();
    let validator_kp = KeyPair::generate();
    let recipient = KeyPair::generate();

    let block = chain.create_block(&validator_kp).unwrap();
    chain.add_block(block).unwrap();

    let sender_address = hash::address_bytes_from_public_key(&validator_kp.public_key);
    let recipient_address = hash::address_bytes_from_public_key(&recipient.public_key);
    let mut tx = Transaction::new(
        chain.chain_id(),
        validator_kp.public_key.clone(),
        recipient_address.clone(),
        1000,
        10,
        0,
    );
    tx.sign(&validator_kp);
    chain.add_transaction(tx).unwrap();

    let block = chain.create_block(&validator_kp).unwrap();
    chain.add_block(block).unwrap();

    assert_eq!(chain.get_balance(&recipient_address), 1000);
    // Sender paid: 1000 transfer + gas fees (base_fee >= 1)
    let sender_balance = chain.get_balance(&sender_address);
    assert!(sender_balance < DEFAULT_BLOCK_REWARD * 2 - 1000);
    assert!(sender_balance > DEFAULT_BLOCK_REWARD * 2 - 1000 - 100_000); // Reasonable fee range
    assert!(chain.is_valid());
}

#[test]
fn test_rejects_forged_mint_transaction() {
    let mut chain = Blockchain::new();
    let attacker = KeyPair::generate();
    let victim = KeyPair::generate();

    let mut tx = Transaction::new(
        chain.chain_id(),
        attacker.public_key.clone(),
        hash::address_bytes_from_public_key(&victim.public_key),
        1_000,
        10,
        0,
    );
    tx.sign(&attacker);

    let err = chain.add_transaction(tx).unwrap_err();
    assert!(matches!(err, ChainError::InsufficientBalance { .. }));
}

#[test]
fn test_stake_locks_funds() {
    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();
    let address = hash::address_bytes_from_public_key(&validator.public_key);

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let mut stake_tx = Transaction::stake(
        chain.chain_id(),
        validator.public_key.clone(),
        10_000_000,
        5,
        0,
    );
    stake_tx.sign(&validator);
    chain.add_transaction(stake_tx).unwrap();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    assert_eq!(chain.get_staked_balance(&address), 10_000_000);
    // Balance = 2 block rewards - staked - gas fees
    let balance = chain.get_balance(&address);
    assert!(balance < DEFAULT_BLOCK_REWARD * 2 - 10_000_000);
    assert!(balance > DEFAULT_BLOCK_REWARD * 2 - 10_000_000 - 100_000);
}

#[test]
fn test_rejects_duplicate_pending_transaction() {
    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();
    let recipient = KeyPair::generate();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let mut tx = Transaction::new(
        chain.chain_id(),
        validator.public_key.clone(),
        hash::address_bytes_from_public_key(&recipient.public_key),
        1000,
        10,
        0,
    );
    tx.sign(&validator);

    chain.add_transaction(tx.clone()).unwrap();
    let err = chain.add_transaction(tx).unwrap_err();
    assert!(matches!(err, ChainError::DuplicateTransaction));
}

#[test]
fn test_mempool_evicts_low_fee_under_gas_pressure() {
    let keypairs: Vec<KeyPair> = (0..9).map(|_| KeyPair::generate()).collect();
    let mut chain = Blockchain::from_genesis(GenesisConfig {
        chain_id: "mempool-pressure-test".to_string(),
        chain_name: "mempool-pressure-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        block_gas_limit: 100_000,
        allocations: keypairs
            .iter()
            .map(|kp| GenesisAllocation {
                public_key: hex::encode(&kp.public_key),
                balance: 10_000_000,
                staked_balance: 0,
            })
            .collect(),
        ..Default::default()
    })
    .unwrap();

    let wasm_code =
        br#"(module (memory (export "memory") 1) (func (export "curs3d_call")))"#.to_vec();
    for kp in keypairs.iter().take(8) {
        let mut tx = Transaction::deploy_contract(
            chain.chain_id(),
            kp.public_key.clone(),
            wasm_code.clone(),
            100_000,
            0,
            0,
        )
        .with_fee_caps(20, 1);
        tx.sign(kp);
        chain.add_transaction(tx).unwrap();
    }

    let premium = keypairs.last().unwrap();
    let mut tx3 = Transaction::deploy_contract(
        chain.chain_id(),
        premium.public_key.clone(),
        wasm_code,
        100_000,
        0,
        0,
    )
    .with_fee_caps(20, 10);
    tx3.sign(premium);
    chain.add_transaction(tx3.clone()).unwrap();

    assert!(!chain.pending_transactions.is_empty());
    assert!(chain.pending_transactions.len() < 9);
    assert!(
        chain
            .pending_transactions
            .iter()
            .any(|pending| pending.hash() == tx3.hash())
    );
    assert!(chain.pending_gas_usage() <= chain.pending_gas_budget());
}

#[test]
fn test_replacement_requires_priority_fee_bump() {
    let validator = KeyPair::generate();
    let recipient = KeyPair::generate();
    let mut chain = Blockchain::from_genesis(GenesisConfig {
        chain_id: "replacement-fee-test".to_string(),
        chain_name: "replacement-fee-test".to_string(),
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 50_000_000,
            staked_balance: 0,
        }],
        ..Default::default()
    })
    .unwrap();

    let recipient_address = hash::address_bytes_from_public_key(&recipient.public_key);
    let mut tx1 = Transaction::new(
        chain.chain_id(),
        validator.public_key.clone(),
        recipient_address.clone(),
        1_000,
        0,
        0,
    )
    .with_fee_caps(10, 2);
    tx1.sign(&validator);
    chain.add_transaction(tx1).unwrap();

    let mut replacement = Transaction::new(
        chain.chain_id(),
        validator.public_key.clone(),
        recipient_address,
        2_000,
        0,
        0,
    )
    .with_fee_caps(20, 2);
    replacement.sign(&validator);
    let err = chain.add_transaction(replacement).unwrap_err();
    assert!(matches!(err, ChainError::ReplacementFeeTooLow));
}

#[test]
fn test_estimate_transaction_reports_fee_breakdown() {
    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();
    let recipient = KeyPair::generate();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let mut tx = Transaction::new(
        chain.chain_id(),
        validator.public_key.clone(),
        hash::address_bytes_from_public_key(&recipient.public_key),
        1_000,
        0,
        0,
    )
    .with_fee_caps(10, 2);
    tx.sign(&validator);

    let estimate = chain.estimate_transaction(&tx).unwrap();
    assert_eq!(estimate.next_block_height, chain.height() + 1);
    assert!(estimate.gas_used > 0);
    assert!(estimate.total_fee_charged > 0);
    assert_eq!(
        estimate.priority_fee_paid + estimate.base_fee_burned + estimate.gas_refunded,
        estimate.max_total_fee
    );
}

#[test]
fn test_rejects_excessive_pending_nonce_gap() {
    let validator = KeyPair::generate();
    let recipient = KeyPair::generate();
    let mut chain = Blockchain::from_genesis(GenesisConfig {
        chain_id: "nonce-gap-test".to_string(),
        chain_name: "nonce-gap-test".to_string(),
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 50_000_000,
            staked_balance: 0,
        }],
        ..Default::default()
    })
    .unwrap();

    let mut tx = Transaction::new(
        chain.chain_id(),
        validator.public_key.clone(),
        hash::address_bytes_from_public_key(&recipient.public_key),
        1_000,
        0,
        MAX_PENDING_NONCE_GAP + 1,
    )
    .with_fee_caps(10, 2);
    tx.sign(&validator);
    let err = chain.add_transaction(tx).unwrap_err();
    assert!(matches!(err, ChainError::InvalidTransactionFormat(_)));
}

#[test]
fn test_unstake_unlocks_funds() {
    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();
    let address = hash::address_bytes_from_public_key(&validator.public_key);

    // Mine a block to get funds
    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    // Stake 10M
    let mut stake_tx = Transaction::stake(
        chain.chain_id(),
        validator.public_key.clone(),
        10_000_000,
        5,
        0,
    );
    stake_tx.sign(&validator);
    chain.add_transaction(stake_tx).unwrap();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    assert_eq!(chain.get_staked_balance(&address), 10_000_000);
    let balance_after_stake = chain.get_balance(&address);

    // Unstake 5M
    let mut unstake_tx = Transaction::unstake(
        chain.chain_id(),
        validator.public_key.clone(),
        5_000_000,
        5,
        1,
    );
    unstake_tx.sign(&validator);
    chain.add_transaction(unstake_tx).unwrap();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    // Staked reduced by 5M and funds remain locked until the unstake delay expires.
    assert_eq!(chain.get_staked_balance(&address), 5_000_000);
    assert_eq!(chain.get_account(&address).pending_unstakes.len(), 1);
    let balance_after_unstake_block = chain.get_balance(&address);
    // Balance includes block reward but minus gas fees for unstake tx
    assert!(balance_after_unstake_block < balance_after_stake + DEFAULT_BLOCK_REWARD - 5_000_000);
    assert!(
        balance_after_unstake_block
            > balance_after_stake + DEFAULT_BLOCK_REWARD - 5_000_000 - 100_000
    );

    for _ in 0..DEFAULT_UNSTAKE_DELAY_BLOCKS {
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();
    }

    assert!(chain.get_balance(&address) >= balance_after_unstake_block + 5_000_000);
}

#[test]
fn test_rejects_invalid_state_root() {
    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();

    let mut block = chain.create_block(&validator).unwrap();
    block.header.state_root = vec![7; 32];
    block.hash = Block::compute_hash(&block.header);
    block.signature = Some(validator.sign(&Block::signable_block_hash(&block.hash)));

    let err = chain.add_block(block).unwrap_err();
    assert!(matches!(err, ChainError::InvalidStateRoot));
}

#[test]
fn test_rejects_unknown_finality_vote_hash() {
    let mut chain = Blockchain::new();
    let voter = KeyPair::generate();
    let vote = FinalityVote::new(hash::sha3_hash(b"unknown"), 1, 0, &voter);
    assert!(chain.add_finality_vote(vote).is_none());
}

#[test]
fn test_rejects_invalid_fork_state_root() {
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-fork-test".to_string(),
        chain_name: "curs3d-fork-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };
    let mut chain = Blockchain::from_genesis(genesis).unwrap();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let mut fork = chain.create_block(&validator).unwrap();
    fork.header.height = 1;
    fork.header.prev_hash = chain.genesis_block().hash;
    fork.header.state_root = vec![7; 32];
    fork.hash = Block::compute_hash(&fork.header);
    fork.signature = Some(validator.sign(&Block::signable_block_hash(&fork.hash)));

    let err = chain.add_block_with_fork_choice(fork).unwrap_err();
    assert!(matches!(err, ChainError::InvalidStateRoot));
}

#[test]
fn test_deploy_contract() {
    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();

    // Mine a block to get funds
    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let wasm_code = br#"(module
            (func (export "curs3d_call") (result i32)
                i32.const 7)
        )"#
    .to_vec();
    let mut deploy_tx = Transaction::deploy_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        wasm_code,
        1_000_000,
        0,
        0,
    )
    .with_fee_caps(20, 2);
    deploy_tx.sign(&validator);
    chain.add_transaction(deploy_tx).unwrap();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    // Verify contract was stored and receipt exists
    assert_eq!(chain.contracts.len(), 1);
    assert!(!chain.receipts.is_empty());

    let receipt = chain.receipts.values().next().unwrap();
    assert!(receipt.success);
    assert!(receipt.contract_address.is_some());
    assert_eq!(receipt.contract_address.as_ref().unwrap().len(), 20);
    assert!(receipt.gas_used > 0);
}

#[test]
fn test_call_contract() {
    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();

    // Mine a block to get funds
    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let wasm_code = br#"(module
            (func (export "curs3d_call") (result i32)
                i32.const 7)
        )"#
    .to_vec();
    let mut deploy_tx = Transaction::deploy_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        wasm_code,
        1_000_000,
        0,
        0,
    )
    .with_fee_caps(20, 2);
    deploy_tx.sign(&validator);
    chain.add_transaction(deploy_tx).unwrap();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    // Get the contract address from the receipt
    let contract_address = chain
        .receipts
        .values()
        .find(|r| r.contract_address.is_some())
        .unwrap()
        .contract_address
        .clone()
        .unwrap();

    // Call the contract
    let mut call_tx = Transaction::call_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        contract_address,
        b"do_something".to_vec(),
        0,
        1_000_000,
        0,
        1,
    )
    .with_fee_caps(20, 3);
    call_tx.sign(&validator);
    chain.add_transaction(call_tx).unwrap();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    // Should have 2 receipts now (deploy + call)
    assert_eq!(chain.receipts.len(), 2);
    let call_receipt = chain
        .receipts
        .values()
        .find(|r| r.contract_address.is_none())
        .unwrap();
    assert!(call_receipt.success);
    assert!(call_receipt.gas_used > 0);
    assert_eq!(call_receipt.return_data, 7i32.to_le_bytes().to_vec());
    assert!(call_receipt.effective_gas_price >= chain.current_base_fee_per_gas());
    assert!(call_receipt.gas_refunded > 0);
    assert_eq!(
        call_receipt.priority_fee_paid + call_receipt.base_fee_burned + call_receipt.gas_refunded,
        20 * 1_000_000
    );
}

#[test]
fn test_async_persistence_does_not_write_on_each_block() {
    let dir = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-async-persist-test".to_string(),
        chain_name: "curs3d-async-persist-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        epoch_length: 8,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };
    let data_dir = dir.path().to_str().unwrap();
    let mut chain = Blockchain::with_storage_async_persistence(data_dir, Some(&genesis)).unwrap();

    let stored_before = chain.storage.as_ref().unwrap().get_height().unwrap();
    assert_eq!(stored_before, Some(0));

    for _ in 0..3 {
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();
    }

    let stored_after = chain.storage.as_ref().unwrap().get_height().unwrap();
    assert_eq!(
        stored_after,
        Some(0),
        "live async mode must not synchronously write sled on each block"
    );
    assert_eq!(chain.height(), 3);
}

#[test]
fn test_async_persistence_drop_timeout_does_not_block_indefinitely() {
    let (sender, receiver) = sync_channel::<PersistJob>(1);
    let handle = thread::spawn(move || {
        let _ = receiver.recv();
        thread::sleep(Duration::from_secs(60));
    });
    let persistence = PersistenceHandle {
        full_state_slot: Arc::new(StdMutex::new(None)),
        signal_sender: sender,
        join_handle: StdMutex::new(Some(handle)),
    };

    let started = std::time::Instant::now();
    drop(persistence);
    assert!(
        started.elapsed() < Duration::from_secs(6),
        "PersistenceHandle::drop must timeout instead of blocking process shutdown"
    );
}

/// Regression for the post-incident audit: write enough blocks to cross
/// at least one epoch boundary, drop the async chain (which triggers
/// `Drop::drop` on `PersistenceHandle` → Shutdown signal → drain →
/// join), reopen, and verify the persisted state matches what was in
/// memory. Catches:
///   - latest-wins FullState slot losing data (it should NEVER lose);
///   - graceful shutdown not actually flushing the slot;
///   - reopen taking a stale snapshot.
#[test]
fn test_async_persistence_reopen_intact_after_epoch_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-async-reopen-test".to_string(),
        chain_name: "curs3d-async-reopen-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        // Small epoch so we cross multiple boundaries inside a unit test.
        epoch_length: 4,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };
    let data_dir = dir.path().to_str().unwrap();

    let last_hash = {
        let mut chain =
            Blockchain::with_storage_async_persistence(data_dir, Some(&genesis)).unwrap();
        // 12 blocks = 3 epochs (boundaries at 4, 8, 12) → at least three
        // FullState signals. The last MUST land on disk.
        for _ in 0..12 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }
        assert_eq!(chain.height(), 12);
        chain.latest_hash().to_vec()
        // chain (and PersistenceHandle) drops here → Shutdown sentinel
        // is sent and the worker joins. The latest FullState in the
        // single-buffered slot is drained on the way out.
    };

    let reopened = Blockchain::with_storage(data_dir, Some(&genesis)).unwrap();
    assert_eq!(
        reopened.height(),
        12,
        "graceful shutdown must drain the FullState slot before exit"
    );
    assert_eq!(
        reopened.latest_hash(),
        last_hash.as_slice(),
        "reopened tip hash must match what was in memory at drop time"
    );
}

/// Regression for "queue full silently drops state". Hammer
/// `persist_full_state` faster than the worker can drain by repeatedly
/// calling it without yielding. The latest-wins slot guarantees the
/// final state lands; older snapshots may be coalesced but are never
/// processed staler than the tip.
#[test]
fn test_async_persistence_full_state_is_latest_wins_under_pressure() {
    let dir = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-async-pressure-test".to_string(),
        chain_name: "curs3d-async-pressure-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        epoch_length: 1, // every block is an epoch boundary
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };
    let data_dir = dir.path().to_str().unwrap();

    let final_hash = {
        let mut chain =
            Blockchain::with_storage_async_persistence(data_dir, Some(&genesis)).unwrap();
        for _ in 0..50 {
            let block = chain.create_block(&validator).unwrap();
            chain.add_block(block).unwrap();
        }
        assert_eq!(chain.height(), 50);
        chain.latest_hash().to_vec()
    };

    let reopened = Blockchain::with_storage(data_dir, Some(&genesis)).unwrap();
    assert_eq!(reopened.height(), 50, "final block must reach disk");
    assert_eq!(reopened.latest_hash(), final_hash.as_slice());
}

/// End-to-end: deploy a real SDK-compiled wasm contract through the chain
/// (not via Vm directly), call it twice, and verify the receipt + state +
/// indexes (block_hash_to_height, tx_hash_index, log_index) all stay
/// consistent. Skipped if the .wasm hasn't been built yet so CI without the
/// wasm32 target keeps passing.
#[test]
fn test_sdk_counter_via_chain_integration() {
    const COUNTER_WASM: &[u8] = include_bytes!(
        "../../../sdk/rust/examples/counter/target/wasm32-unknown-unknown/release/counter_contract.wasm"
    );
    if COUNTER_WASM.len() < 16 {
        return;
    }

    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();
    // Mine several blocks to give the validator enough liquid balance to pay
    // for the deploy + call gas budgets.
    for _ in 0..6 {
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();
    }

    // Deploy the SDK contract via a real DeployContract tx
    let mut deploy_tx = Transaction::deploy_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        COUNTER_WASM.to_vec(),
        5_000_000,
        0,
        0,
    )
    .with_fee_caps(50, 5);
    deploy_tx.sign(&validator);
    let deploy_hash = deploy_tx.hash();
    chain.add_transaction(deploy_tx).unwrap();
    let block = chain.create_block(&validator).unwrap();
    let deploy_block_hash = block.hash.clone();
    let deploy_block_height = block.header.height;
    chain.add_block(block).unwrap();

    // Index integrity: deploy block + tx are both reachable in O(1)
    assert_eq!(
        chain.block_hash_to_height.get(&deploy_block_hash).copied(),
        Some(deploy_block_height)
    );
    assert_eq!(
        chain.tx_hash_index.get(&deploy_hash).map(|(h, _)| *h),
        Some(deploy_block_height)
    );

    let contract_address = chain
        .receipts
        .values()
        .find(|r| r.contract_address.is_some())
        .expect("deploy receipt missing")
        .contract_address
        .clone()
        .unwrap();

    // First call: counter goes 0 → 1
    let mut call1 = Transaction::call_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        contract_address.clone(),
        Vec::new(),
        0,
        2_000_000,
        0,
        1,
    )
    .with_fee_caps(50, 5);
    call1.sign(&validator);
    let call1_hash = call1.hash();
    chain.add_transaction(call1).unwrap();
    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let receipt1 = chain
        .get_receipt(&call1_hash)
        .expect("call1 receipt missing");
    assert!(receipt1.receipt.success);
    assert_eq!(receipt1.receipt.logs.len(), 1);
    assert_eq!(receipt1.receipt.logs[0].topics[0], b"tick");
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&receipt1.receipt.logs[0].data[..8]);
    assert_eq!(u64::from_le_bytes(buf), 1);

    // Second call: counter goes 1 → 2 (proves storage persisted across blocks)
    let mut call2 = Transaction::call_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        contract_address.clone(),
        Vec::new(),
        0,
        2_000_000,
        0,
        2,
    )
    .with_fee_caps(50, 5);
    call2.sign(&validator);
    let call2_hash = call2.hash();
    chain.add_transaction(call2).unwrap();
    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let receipt2 = chain
        .get_receipt(&call2_hash)
        .expect("call2 receipt missing");
    let mut buf2 = [0u8; 8];
    buf2.copy_from_slice(&receipt2.receipt.logs[0].data[..8]);
    assert_eq!(u64::from_le_bytes(buf2), 2);

    // Log index gets two `tick` entries on the same contract
    let logs = chain.query_logs(&LogFilter {
        contract: Some(contract_address),
        topic: Some(b"tick".to_vec()),
        topics: None,
        from_block: None,
        to_block: None,
        limit: Some(10),
    });
    assert_eq!(logs.len(), 2);
}

#[test]
fn test_deploy_contract_rejects_oversize_wasm() {
    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();
    // Mine a block to fund the validator
    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    // 257 KB of bytes prefixed with the wasm magic so the size check fires
    // before any wasm validation logic.
    let mut oversize = vec![0u8; MAX_CONTRACT_CODE_BYTES + 1024];
    oversize[..4].copy_from_slice(b"\0asm");
    let mut tx = Transaction::deploy_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        oversize,
        5_000_000,
        0,
        0,
    )
    .with_fee_caps(50, 5);
    tx.sign(&validator);

    let err = chain.add_transaction(tx).unwrap_err();
    match err {
        ChainError::InvalidTransactionFormat(msg) => {
            assert!(
                msg.contains("256 KB"),
                "expected size-limit error, got: {}",
                msg
            );
        }
        other => panic!("expected InvalidTransactionFormat, got {:?}", other),
    }
}

#[test]
fn test_block_hash_index_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();

    let allocations = vec![GenesisAllocation {
        public_key: hex::encode(&validator.public_key),
        balance: 1_000_000_000,
        staked_balance: 0,
    }];
    let genesis_config = GenesisConfig {
        allocations,
        ..GenesisConfig::default()
    };

    let mut block_hashes = Vec::new();
    {
        let mut chain =
            Blockchain::with_storage(dir.path().to_str().unwrap(), Some(&genesis_config)).unwrap();
        for _ in 0..3 {
            let block = chain.create_block(&validator).unwrap();
            block_hashes.push(block.hash.clone());
            chain.add_block(block).unwrap();
        }
        // Index populated in-memory after add_block
        for (i, h) in block_hashes.iter().enumerate() {
            assert_eq!(
                chain.block_hash_to_height.get(h).copied(),
                Some((i + 1) as u64)
            );
        }
    }
    // Reopen from disk: rebuild_canonical_state must repopulate the index
    let chain =
        Blockchain::with_storage(dir.path().to_str().unwrap(), Some(&genesis_config)).unwrap();
    for (i, h) in block_hashes.iter().enumerate() {
        assert_eq!(
            chain.block_hash_to_height.get(h).copied(),
            Some((i + 1) as u64),
            "block hash index must survive restart"
        );
    }
}

#[test]
fn test_account_and_storage_proofs_roundtrip() {
    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let wasm_code = br#"(module
            (import "curs3d" "storage_write_bytes" (func $storage_write_bytes (param i32 i32 i32 i32) (result i32)))
            (import "curs3d" "storage_read" (func $storage_read (param i32 i32 i32 i32) (result i32)))
            (import "curs3d" "emit_log_bytes" (func $emit_log_bytes (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "key1")
            (data (i32.const 16) "val1")
            (data (i32.const 32) "topic")
            (func (export "curs3d_call") (result i32)
                i32.const 0
                i32.const 4
                i32.const 16
                i32.const 4
                call $storage_write_bytes
                drop
                i32.const 0
                i32.const 4
                i32.const 64
                i32.const 16
                call $storage_read
                drop
                i32.const 32
                i32.const 5
                i32.const 64
                i32.const 4
                call $emit_log_bytes
                drop
                i32.const 1)
        )"#
        .to_vec();

    let mut deploy_tx = Transaction::deploy_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        wasm_code,
        1_000_000,
        0,
        0,
    )
    .with_fee_caps(20, 2);
    deploy_tx.sign(&validator);
    chain.add_transaction(deploy_tx).unwrap();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let contract_address = chain
        .receipts
        .values()
        .find_map(|receipt| receipt.contract_address.clone())
        .unwrap();

    let mut call_tx = Transaction::call_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        contract_address.clone(),
        Vec::new(),
        0,
        1_000_000,
        0,
        1,
    )
    .with_fee_caps(20, 2);
    let call_tx_hash = call_tx.hash();
    call_tx.sign(&validator);
    chain.add_transaction(call_tx).unwrap();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let validator_address = hash::address_bytes_from_public_key(&validator.public_key);
    let account_proof = chain.get_account_proof(&validator_address).unwrap();
    assert!(Blockchain::verify_account_proof(&account_proof));

    let storage_proof = chain.get_storage_proof(&contract_address, b"key1").unwrap();
    assert_eq!(storage_proof.value, b"val1".to_vec());
    assert!(Blockchain::verify_storage_proof(&storage_proof));

    let indexed_receipt = chain.get_receipt(&call_tx_hash).unwrap();
    assert_eq!(indexed_receipt.block_height, 3);
    assert_eq!(indexed_receipt.receipt.logs.len(), 1);

    let logs = chain.query_logs(&LogFilter {
        contract: Some(contract_address.clone()),
        topic: Some(b"topic".to_vec()),
        topics: None,
        from_block: Some(0),
        to_block: Some(chain.height()),
        limit: Some(10),
    });
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].tx_hash, call_tx_hash);
    assert_eq!(logs[0].data, b"val1".to_vec());

    // Positional topics filter (eth-style): topic at index 0 must equal "topic"
    let logs_positional = chain.query_logs(&LogFilter {
        contract: Some(contract_address.clone()),
        topic: None,
        topics: Some(vec![Some(b"topic".to_vec())]),
        from_block: None,
        to_block: None,
        limit: Some(10),
    });
    assert_eq!(logs_positional.len(), 1);

    // Wildcard at position 0 matches everything
    let logs_wildcard = chain.query_logs(&LogFilter {
        contract: Some(contract_address.clone()),
        topic: None,
        topics: Some(vec![None]),
        from_block: None,
        to_block: None,
        limit: Some(10),
    });
    assert_eq!(logs_wildcard.len(), 1);

    // Wrong topic at position 0 matches nothing
    let logs_no_match = chain.query_logs(&LogFilter {
        contract: Some(contract_address),
        topic: None,
        topics: Some(vec![Some(b"other".to_vec())]),
        from_block: None,
        to_block: None,
        limit: Some(10),
    });
    assert_eq!(logs_no_match.len(), 0);
}

#[test]
fn test_transactions_for_address_returns_sent_and_received() {
    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();

    // Block 1: validator gets the coinbase reward
    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    // Validator sends a transfer to a fresh address
    let recipient = vec![9u8; hash::ADDRESS_LEN];
    let mut transfer_tx = Transaction::new(
        chain.chain_id(),
        validator.public_key.clone(),
        recipient.clone(),
        1_000,
        100,
        0,
    );
    transfer_tx.sign(&validator);
    let transfer_hash = transfer_tx.hash();
    chain.add_transaction(transfer_tx).unwrap();
    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let validator_addr = hash::address_bytes_from_public_key(&validator.public_key);
    let validator_txs = chain.transactions_for_address(&validator_addr, None, None, 50);
    // Validator was sender of the transfer + recipient of coinbase blocks
    assert!(
        validator_txs
            .iter()
            .any(|(_, _, tx)| tx.hash() == transfer_hash),
        "should include the transfer the validator sent"
    );

    let recipient_txs = chain.transactions_for_address(&recipient, None, None, 50);
    assert_eq!(recipient_txs.len(), 1);
    assert_eq!(recipient_txs[0].2.hash(), transfer_hash);

    // Limit clamps the result
    let limited = chain.transactions_for_address(&validator_addr, None, None, 1);
    assert_eq!(limited.len(), 1);
}

#[test]
fn test_gas_limit_exceeded() {
    let mut chain = Blockchain::new();
    let validator = KeyPair::generate();

    // Mine a block to get funds
    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    // Try to deploy with gas_limit too low
    let wasm_code = b"\0asm\x01\x00\x00\x00".to_vec();
    let mut deploy_tx = Transaction::deploy_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        wasm_code,
        100, // way too low
        100,
        0,
    );
    deploy_tx.sign(&validator);

    let err = chain.add_transaction(deploy_tx).unwrap_err();
    assert!(matches!(err, ChainError::VmError(_)));
}

#[test]
fn test_epoch_snapshot_created_at_boundary() {
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-epoch-test".to_string(),
        chain_name: "curs3d-epoch-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: 4, // short epoch for testing
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };
    let mut chain = Blockchain::from_genesis(genesis).unwrap();

    // Mine 4 blocks to hit epoch boundary
    for _ in 0..4 {
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();
    }

    // After height 4, epoch 1 should have a snapshot
    assert!(chain.epoch_snapshots.contains_key(&1));
    let snapshot = chain.epoch_snapshots.get(&1).unwrap();
    assert_eq!(snapshot.epoch, 1);
    assert_eq!(snapshot.start_height, 4);
    assert!(!snapshot.validators.is_empty());
    assert!(snapshot.total_stake > 0);
}

#[test]
fn test_validator_selection_uses_frozen_set() {
    use crate::consensus::ProofOfStake;

    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-frozen-test".to_string(),
        chain_name: "curs3d-frozen-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: 4,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };
    let mut chain = Blockchain::from_genesis(genesis).unwrap();

    // Mine 4 blocks to create epoch snapshot
    for _ in 0..4 {
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();
    }

    let snapshot = chain.epoch_snapshots.get(&1).unwrap();
    // The frozen set should contain the validator
    assert_eq!(snapshot.validators.len(), 1);
    assert_eq!(snapshot.validators[0].public_key, validator.public_key);

    // select_validator_from_snapshot should return the validator
    let selected = ProofOfStake::select_validator_from_snapshot(snapshot, 5, &chain.latest_hash());
    assert!(selected.is_some());
    assert_eq!(selected.unwrap().public_key, validator.public_key);
}

#[test]
fn test_snapshot_create_and_verify() {
    let dir = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-snapshot-test".to_string(),
        chain_name: "curs3d-snapshot-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };
    let mut chain = Blockchain::with_storage(dir.path().to_str().unwrap(), Some(&genesis)).unwrap();

    // Mine a few blocks
    for _ in 0..3 {
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();
    }

    let wasm_code = br#"(module
            (func (export "curs3d_call") (result i32)
                i32.const 9)
        )"#
    .to_vec();
    let mut deploy_tx = Transaction::deploy_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        wasm_code,
        1_000_000,
        100,
        0,
    );
    deploy_tx.sign(&validator);
    chain.add_transaction(deploy_tx).unwrap();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    // Create a snapshot
    let manifest = chain.create_snapshot().unwrap();
    let chunks = chain.get_snapshot_chunks(manifest.height).unwrap();
    assert_eq!(manifest.height, 4);
    assert!(manifest.chunk_count > 0);
    assert_eq!(manifest.chunk_hashes.len(), manifest.chunk_count);

    // Verify state root matches
    let expected_root = Blockchain::compute_state_root_full(&chain.accounts, &chain.contracts);
    assert_eq!(manifest.state_root, expected_root);

    let mut restored = Blockchain::from_genesis(genesis).unwrap();
    restored.apply_snapshot(&manifest, &chunks).unwrap();
    assert_eq!(restored.accounts, chain.accounts);
    assert_eq!(restored.contracts, chain.contracts);
    assert_eq!(restored.receipts.len(), chain.receipts.len());
    for (tx_hash, receipt) in &chain.receipts {
        let restored_receipt = restored.receipts.get(tx_hash).unwrap();
        assert_eq!(restored_receipt.success, receipt.success);
        assert_eq!(restored_receipt.gas_used, receipt.gas_used);
        assert_eq!(restored_receipt.contract_address, receipt.contract_address);
        assert_eq!(restored_receipt.return_data, receipt.return_data);
    }
    assert_eq!(restored.height(), chain.height());
}

#[test]
fn test_snapshot_uses_finalized_base_and_tracks_tip() {
    let validator = KeyPair::generate();
    let mut chain = Blockchain::from_genesis(GenesisConfig {
        chain_id: "snapshot-finalized-test".to_string(),
        chain_name: "snapshot-finalized-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    })
    .unwrap();

    let block1 = chain.create_block(&validator).unwrap();
    chain.add_block(block1.clone()).unwrap();
    chain
        .block_tree
        .set_finalized(block1.hash.clone(), block1.header.height);
    chain.finality_tracker =
        FinalityTracker::with_finalized(block1.header.height, block1.hash.clone());

    let block2 = chain.create_block(&validator).unwrap();
    chain.add_block(block2.clone()).unwrap();

    let manifest = chain.create_snapshot().unwrap();
    assert_eq!(manifest.height, 1);
    assert_eq!(manifest.latest_hash, block1.hash);
    assert_eq!(manifest.tip_height, 2);
    assert_eq!(manifest.tip_hash, block2.hash);
}

#[test]
fn test_snapshot_rejects_tampered_chunk_proof() {
    let dir = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-snapshot-proof-test".to_string(),
        chain_name: "curs3d-snapshot-proof-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };
    let mut chain = Blockchain::with_storage(dir.path().to_str().unwrap(), Some(&genesis)).unwrap();
    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let manifest = chain.create_snapshot().unwrap();
    let mut chunks = chain.get_snapshot_chunks(manifest.height).unwrap();
    if let Some(first) = chunks.first_mut() {
        if let Some(proof_hash) = first.proof.first_mut() {
            proof_hash[0] ^= 0xFF;
        } else {
            first.proof.push(vec![0xAA; 32]);
        }
    }

    let mut restored = Blockchain::from_genesis(genesis).unwrap();
    let err = restored.apply_snapshot(&manifest, &chunks).unwrap_err();
    assert!(matches!(err, ChainError::SnapshotError(_)));
}

/// Regression: divergent non-finalized local forks are recoverable by
/// snapshot sync. This is how a restarted/late node escapes a local fork
/// without an operator wipe, while finalized checkpoints remain protected
/// by the next test.
#[test]
fn test_snapshot_replaces_divergent_non_finalized_suffix() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-snapshot-divergent-test".to_string(),
        chain_name: "curs3d-snapshot-divergent-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };

    // chain_a = our local node, only one block past genesis.
    let mut chain_a =
        Blockchain::with_storage(dir_a.path().to_str().unwrap(), Some(&genesis)).unwrap();
    let block = chain_a.create_block(&validator).unwrap();
    chain_a.add_block(block).unwrap();
    let local_block_1_hash = chain_a.block_at_height(1).unwrap().hash;

    // chain_b = remote peer with a divergent history, several blocks ahead.
    // To force divergence at height 1 (block creation is otherwise deterministic
    // when the validator and parent are identical), wait one second so the
    // block timestamp differs.
    std::thread::sleep(std::time::Duration::from_secs(1));
    let mut chain_b =
        Blockchain::with_storage(dir_b.path().to_str().unwrap(), Some(&genesis)).unwrap();
    for _ in 0..5 {
        let block = chain_b.create_block(&validator).unwrap();
        chain_b.add_block(block).unwrap();
    }
    // Sanity: chain_b's block at height 1 must disagree with chain_a's.
    assert_ne!(chain_b.block_at_height(1).unwrap().hash, local_block_1_hash);
    // chain_a is shorter than chain_b's snapshot tip, so the legacy
    // tip-only check cannot fire here.
    assert!(chain_a.height() < chain_b.height());

    let manifest_b = chain_b.create_snapshot().unwrap();
    let chunks_b = chain_b.get_snapshot_chunks(manifest_b.height).unwrap();
    chain_a.apply_snapshot(&manifest_b, &chunks_b).unwrap();
    assert_eq!(chain_a.height(), chain_b.height());
    assert_ne!(chain_a.block_at_height(1).unwrap().hash, local_block_1_hash);
    assert_eq!(
        chain_a.block_at_height(1).unwrap().hash,
        chain_b.block_at_height(1).unwrap().hash
    );
}

#[test]
fn test_snapshot_rejects_divergent_finalized_checkpoint() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-snapshot-divergent-finalized".to_string(),
        chain_name: "curs3d-snapshot-divergent-finalized".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };

    let mut chain_a =
        Blockchain::with_storage(dir_a.path().to_str().unwrap(), Some(&genesis)).unwrap();
    let block = chain_a.create_block(&validator).unwrap();
    chain_a.add_block(block).unwrap();
    let local_block_1_hash = chain_a.block_at_height(1).unwrap().hash;
    chain_a
        .block_tree
        .set_finalized(local_block_1_hash.clone(), 1);
    chain_a.finality_tracker = FinalityTracker::with_finalized(1, local_block_1_hash.clone());

    std::thread::sleep(std::time::Duration::from_secs(1));
    let mut chain_b =
        Blockchain::with_storage(dir_b.path().to_str().unwrap(), Some(&genesis)).unwrap();
    for _ in 0..5 {
        let block = chain_b.create_block(&validator).unwrap();
        chain_b.add_block(block).unwrap();
    }
    assert_ne!(chain_b.block_at_height(1).unwrap().hash, local_block_1_hash);

    let manifest_b = chain_b.create_snapshot().unwrap();
    let chunks_b = chain_b.get_snapshot_chunks(manifest_b.height).unwrap();
    let err = chain_a.apply_snapshot(&manifest_b, &chunks_b).unwrap_err();
    assert!(matches!(err, ChainError::SnapshotError(_)));
    assert_eq!(chain_a.block_at_height(1).unwrap().hash, local_block_1_hash);
}

#[test]
fn test_restart_restores_contracts_and_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-restart-test".to_string(),
        chain_name: "curs3d-restart-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };

    let data_dir = dir.path().join("chain_db");
    let data_dir_str = data_dir.to_str().unwrap();

    let mut chain = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let wasm_code = br#"(module
            (func (export "curs3d_call") (result i32)
                i32.const 11)
        )"#
    .to_vec();
    let mut deploy_tx = Transaction::deploy_contract(
        chain.chain_id(),
        validator.public_key.clone(),
        wasm_code,
        1_000_000,
        100,
        0,
    );
    deploy_tx.sign(&validator);
    chain.add_transaction(deploy_tx).unwrap();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let expected_contracts = chain.contracts.clone();
    let expected_receipts = chain.receipts.clone();
    let expected_height = chain.height();

    drop(chain);

    let restarted = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
    assert_eq!(restarted.height(), expected_height);
    assert_eq!(restarted.contracts, expected_contracts);
    assert_eq!(restarted.receipts.len(), expected_receipts.len());
    for (tx_hash, receipt) in expected_receipts {
        let restored = restarted.receipts.get(&tx_hash).unwrap();
        assert_eq!(restored.success, receipt.success);
        assert_eq!(restored.gas_used, receipt.gas_used);
        assert_eq!(restored.contract_address, receipt.contract_address);
    }
}

/// Regression test for #2: build a chain, drop it, reload from disk, and
/// verify the recomputed state root matches every persisted block header.
/// If a non-determinism creeps into state-root computation (HashMap
/// iteration order, leaky governance state, etc.) this will fail with
/// `InvalidStateRoot` on the second `with_storage`.
#[test]
fn test_state_root_deterministic_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();
    let recipient = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-state-root-restart".to_string(),
        chain_name: "curs3d-state-root-restart".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![
            GenesisAllocation {
                public_key: hex::encode(&validator.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            },
            GenesisAllocation {
                public_key: hex::encode(&recipient.public_key),
                balance: 0,
                staked_balance: 0,
            },
        ],
        ..Default::default()
    };

    let data_dir = dir.path().join("chain_db");
    let data_dir_str = data_dir.to_str().unwrap();

    let mut chain = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
    // Build a few blocks with mixed activity (transfer + stake unwinding +
    // contract deploy) so the state graph has enough surface area to
    // expose non-determinism if any creeps in.
    let recipient_addr = hash::address_bytes_from_public_key(&recipient.public_key);
    for i in 0..5u64 {
        let mut tx = Transaction::new(
            chain.chain_id(),
            validator.public_key.clone(),
            recipient_addr.clone(),
            100,
            10,
            i,
        );
        tx.sign(&validator);
        chain.add_transaction(tx).unwrap();
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();
    }
    let expected_height = chain.height();
    let expected_state_roots: Vec<Vec<u8>> =
        chain.iter_blocks().map(|b| b.header.state_root).collect();

    drop(chain);

    // Reloading must succeed; rebuild_canonical_state would otherwise
    // raise InvalidStateRoot during replay.
    let restarted = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
    assert_eq!(restarted.height(), expected_height);
    for (h, expected_root) in expected_state_roots.iter().enumerate() {
        assert_eq!(
            &restarted
                .block_at_height(h as u64)
                .unwrap()
                .header
                .state_root,
            expected_root,
            "state_root for block {} diverged after restart",
            h
        );
    }
}

#[test]
fn test_restart_restores_token_registry_and_governance() {
    let dir = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-governance-token-test".to_string(),
        chain_name: "curs3d-governance-token-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };

    let data_dir = dir.path().join("chain_db");
    let data_dir_str = data_dir.to_str().unwrap();
    let mut chain = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();

    let mut deploy_token_tx = Transaction::new(
        chain.chain_id(),
        validator.public_key.clone(),
        Vec::new(),
        0,
        100,
        0,
    );
    deploy_token_tx.kind = TransactionKind::DeployToken;
    deploy_token_tx.data = serde_json::to_vec(&crate::token::DeployTokenParams {
        name: "Persisted Token".to_string(),
        symbol: "PST".to_string(),
        decimals: 6,
        total_supply: 1_000_000,
    })
    .unwrap();
    deploy_token_tx.sign(&validator);
    chain.add_transaction(deploy_token_tx).unwrap();

    let mut proposal_tx = Transaction::new(
        chain.chain_id(),
        validator.public_key.clone(),
        Vec::new(),
        0,
        100,
        1,
    );
    proposal_tx.kind = TransactionKind::SubmitProposal;
    proposal_tx.data = serde_json::to_vec(&crate::governance::SubmitProposalParams {
        kind: crate::governance::ProposalKind::ParameterChange {
            parameter: "block_gas_limit".to_string(),
            new_value: 20_000_000,
        },
    })
    .unwrap();
    proposal_tx.sign(&validator);
    chain.add_transaction(proposal_tx).unwrap();

    let block = chain.create_block(&validator).unwrap();
    chain.add_block(block).unwrap();

    let expected_registry = chain.token_registry.clone();
    let expected_governance_ids: Vec<Vec<u8>> = chain
        .governance
        .list_proposals()
        .iter()
        .map(|p| p.id.clone())
        .collect();

    assert_eq!(expected_registry.tokens.len(), 1);
    assert_eq!(expected_governance_ids.len(), 1);

    drop(chain);

    let restarted = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
    assert_eq!(restarted.token_registry, expected_registry);
    let restarted_ids: Vec<Vec<u8>> = restarted
        .governance
        .list_proposals()
        .iter()
        .map(|p| p.id.clone())
        .collect();
    assert_eq!(restarted_ids, expected_governance_ids);
}

#[test]
fn test_protocol_version_at_height() {
    let genesis = GenesisConfig {
        chain_id: "curs3d-version-test".to_string(),
        chain_name: "curs3d-version-test".to_string(),
        upgrades: vec![
            super::ProtocolUpgrade {
                height: 10,
                version: 2,
                description: "Version 2 upgrade".to_string(),
            },
            super::ProtocolUpgrade {
                height: 20,
                version: 3,
                description: "Version 3 upgrade".to_string(),
            },
        ],
        ..Default::default()
    };
    let chain = Blockchain::from_genesis(genesis).unwrap();

    assert_eq!(chain.protocol_version_at_height(0), 1);
    assert_eq!(chain.protocol_version_at_height(5), 1);
    assert_eq!(chain.protocol_version_at_height(10), 2);
    assert_eq!(chain.protocol_version_at_height(15), 2);
    assert_eq!(chain.protocol_version_at_height(20), 3);
    assert_eq!(chain.protocol_version_at_height(100), 3);
}

#[test]
fn test_rejects_wrong_protocol_version() {
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-ver-reject-test".to_string(),
        chain_name: "curs3d-ver-reject-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };
    let mut chain = Blockchain::from_genesis(genesis).unwrap();

    // Create a valid block then tamper with its version
    let mut block = chain.create_block(&validator).unwrap();
    block.header.version = 99; // Wrong version
    block.hash = Block::compute_hash(&block.header);
    block.signature = Some(validator.sign(&Block::signable_block_hash(&block.hash)));

    let err = chain.add_block(block).unwrap_err();
    assert!(matches!(err, ChainError::InvalidProtocolVersion { .. }));
}

#[test]
fn test_block_rejected_if_wrong_proposer() {
    // Two validators in genesis with equal stake. After the legitimate
    // leader produces block 1 (advancing the parent timestamp to ~now),
    // the non-leader at height 2 must be rejected with WrongProposer
    // because no backup-leader timeout has elapsed yet.
    let kp_a = KeyPair::generate();
    let kp_b = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-wrong-proposer-test".to_string(),
        chain_name: "curs3d-wrong-proposer-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![
            GenesisAllocation {
                public_key: hex::encode(&kp_a.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            },
            GenesisAllocation {
                public_key: hex::encode(&kp_b.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            },
        ],
        ..Default::default()
    };
    let mut chain = Blockchain::from_genesis(genesis).unwrap();

    // Mine block 1 with whichever validator is elected at height 1. This
    // refreshes the parent timestamp to ~now so the rank-0 window is
    // active for height 2.
    let leader_h1 = chain
        .slot_leader_address(1, &chain.latest_hash(), 0)
        .expect("slot leader at height 1");
    let addr_a = hash::address_bytes_from_public_key(&kp_a.public_key);
    let real_kp_h1 = if leader_h1 == addr_a { &kp_a } else { &kp_b };
    let block1 = chain.create_block(real_kp_h1).unwrap();
    chain.add_block(block1).unwrap();

    // Now identify the rank-0 leader at height 2 and pick the *other*
    // keypair as the imposter.
    let leader_h2 = chain
        .slot_leader_address(2, &chain.latest_hash(), 0)
        .expect("slot leader at height 2");
    let imposter_kp = if leader_h2 == addr_a { &kp_b } else { &kp_a };

    // The non-leader's create_block fails fast with WrongProposer.
    let err = chain
        .create_block(imposter_kp)
        .expect_err("non-leader should not be allowed to produce");
    assert!(
        matches!(err, ChainError::WrongProposer { height: 2, .. }),
        "expected WrongProposer at height 2, got {:?}",
        err
    );
}

#[test]
fn test_height_one_is_primary_only_even_if_genesis_timestamp_is_old() {
    let kp_a = KeyPair::generate();
    let kp_b = KeyPair::generate();
    let addr_a = hash::address_bytes_from_public_key(&kp_a.public_key);
    let genesis = GenesisConfig {
        chain_id: "curs3d-height-one-primary-only".to_string(),
        chain_name: "curs3d-height-one-primary-only".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![
            GenesisAllocation {
                public_key: hex::encode(&kp_a.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            },
            GenesisAllocation {
                public_key: hex::encode(&kp_b.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            },
        ],
        ..Default::default()
    };
    let chain = Blockchain::from_genesis(genesis).unwrap();
    let leader_h1 = chain
        .slot_leader_address(1, &chain.latest_hash(), 0)
        .expect("height-1 leader");
    let backup_kp = if leader_h1 == addr_a { &kp_b } else { &kp_a };
    let err = chain
        .create_block(backup_kp)
        .expect_err("height-1 backup must not be authorized");
    assert!(matches!(err, ChainError::WrongProposer { height: 1, .. }));
}

#[test]
fn test_two_validators_alternate() {
    // Sample a long run of slot-leader picks across two equal-stake
    // validators. Verify a single block per height (no fork) and a roughly
    // 50/50 split. With only `slot_leader` as the gate, both validators
    // converge to the same producer per height.
    use crate::consensus::slot_leader;
    let kp_a = KeyPair::generate();
    let kp_b = KeyPair::generate();
    let addr_a = hash::address_bytes_from_public_key(&kp_a.public_key);
    let addr_b = hash::address_bytes_from_public_key(&kp_b.public_key);
    let genesis = GenesisConfig {
        chain_id: "curs3d-two-validator-test".to_string(),
        chain_name: "curs3d-two-validator-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![
            GenesisAllocation {
                public_key: hex::encode(&kp_a.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            },
            GenesisAllocation {
                public_key: hex::encode(&kp_b.public_key),
                balance: 1_000_000_000,
                staked_balance: 5_000,
            },
        ],
        ..Default::default()
    };
    let chain = Blockchain::from_genesis(genesis).unwrap();
    // Walk a synthetic 30-slot ledger. Each height: only the elected
    // leader produces — exactly one block per height — and the other
    // validator stays quiet. 30 samples make a degenerate 30/0 split
    // astronomically unlikely (P < 1e-9) for an honest hash function.
    // We go through `slot_leader_address` so the per-height live-snapshot
    // fallback applies (the genesis-epoch cached snapshot is empty by
    // construction; see `snapshot_for_height`).
    let _ = slot_leader; // imported for symmetry with the bare-snapshot API
    let mut leaders: Vec<Vec<u8>> = Vec::new();
    let parent = chain.latest_hash().to_vec();
    for h in 1..=30 {
        let leader = chain.slot_leader_address(h, &parent, 0).unwrap();
        leaders.push(leader);
    }

    // Single producer per height (vacuously true here, but proves the
    // function returns a unique address per slot).
    for leader in &leaders {
        assert!(*leader == addr_a || *leader == addr_b);
    }

    let count_a = leaders.iter().filter(|a| **a == addr_a).count();
    let count_b = leaders.iter().filter(|a| **a == addr_b).count();
    assert_eq!(count_a + count_b, 30);
    // 50/50 in expectation; allow any non-degenerate split.
    assert!(count_a > 0, "validator A never produced");
    assert!(count_b > 0, "validator B never produced");
    // Ratio within ±50% of expected 15 (very loose to absorb sampling).
    assert!(
        (5..=25).contains(&count_a),
        "split too skewed: a={}, b={}",
        count_a,
        count_b
    );
}

/// Regression test for the `state_root_mismatch` bug at the second epoch
/// boundary on multi-validator chains.
///
/// Symptom: a node that has lived past `h = 2 * epoch_length` (the first
/// height where epoch settlement actually distributes rewards — the
/// `prev_epoch > 0` guard in `add_block` skips settlement at the very
/// first epoch boundary `h = epoch_length`) crashes on restart with
/// `Failed to initialize blockchain storage: invalid state root`.
///
/// Root cause: `add_block` mutates `self.accounts` via
/// `consensus::apply_epoch_settlement` *before* calling
/// `validate_block_against_state` — the live state root therefore reflects
/// post-settlement balances. But the boot path
/// (`with_storage` -> `rebuild_canonical_state`) replays each block via
/// `validate_block_against_state` *without* applying the same settlement
/// step on the parent accounts handed in. The recomputed state root for
/// the boundary block is therefore strictly less (rewards never granted)
/// and the chain refuses to load.
///
/// This test reproduces the failure with a 2-validator genesis (similar
/// to the live testnet) and a small `epoch_length` of 4, mining past
/// `h = 2 * epoch_length = 8` so that the *second* epoch boundary
/// settles non-zero rewards. With the bug, the second `with_storage`
/// returns `ChainError::InvalidStateRoot`. With the fix it succeeds.
#[test]
fn test_restart_across_epoch_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let kp_a = KeyPair::generate();
    let kp_b = KeyPair::generate();
    let addr_a = hash::address_bytes_from_public_key(&kp_a.public_key);
    // Use a tiny epoch length so we cross the *second* boundary quickly.
    // EPOCH_REWARD_RATE_PER_CUR is 100 microtokens per CUR per block, so
    // we need staked >= 1 CUR (= 1_000_000 microtokens) for rewards to
    // be non-zero — the bug is otherwise masked by the saturating
    // arithmetic.
    let epoch_length: u64 = 4;
    let stake = 1_000_000_000u64; // 1000 CUR — comfortably above minimum_stake.
    let genesis = GenesisConfig {
        chain_id: "curs3d-restart-epoch-test".to_string(),
        chain_name: "curs3d-restart-epoch-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        allocations: vec![
            GenesisAllocation {
                public_key: hex::encode(&kp_a.public_key),
                balance: 1_000_000_000,
                staked_balance: stake,
            },
            GenesisAllocation {
                public_key: hex::encode(&kp_b.public_key),
                balance: 1_000_000_000,
                staked_balance: stake,
            },
        ],
        ..Default::default()
    };
    let data_dir = dir.path().join("chain_db");
    let data_dir_str = data_dir.to_str().unwrap();

    // Mine across the first *two* epoch boundaries so the bug actually
    // fires. Settlement at h=epoch_length is skipped (prev_epoch == 0),
    // settlement at h=2*epoch_length runs and grants rewards. After that
    // any restart re-validates block 2*epoch_length and trips the bug.
    let target_height = 2 * epoch_length + 1;
    let expected_state_roots: Vec<Vec<u8>>;
    let expected_height: u64;
    {
        let mut chain = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
        for h in 1..=target_height {
            let leader = chain
                .slot_leader_address(h, &chain.latest_hash(), 0)
                .expect("slot leader exists");
            let kp = if leader == addr_a { &kp_a } else { &kp_b };
            let block = chain.create_block(kp).unwrap();
            chain.add_block(block).unwrap();
        }
        expected_height = chain.height();
        expected_state_roots = chain.iter_blocks().map(|b| b.header.state_root).collect();
        assert_eq!(expected_height, target_height);
    }

    // Reload — this is what fails with `invalid state root` in the wild.
    let restarted = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
    assert_eq!(restarted.height(), expected_height);
    for (h, expected_root) in expected_state_roots.iter().enumerate() {
        assert_eq!(
            &restarted
                .block_at_height(h as u64)
                .unwrap()
                .header
                .state_root,
            expected_root,
            "state_root for block {} diverged after restart",
            h
        );
    }
    // Open a third time as paranoia: ensure replay is idempotent.
    drop(restarted);
    let restarted_again = Blockchain::with_storage(data_dir_str, Some(&genesis)).unwrap();
    assert_eq!(restarted_again.height(), expected_height);
}

// ───────── Mempool priority classes ─────────

#[test]
fn mempool_class_assignment_per_kind() {
    // System: validator-set or governance-affecting.
    assert_eq!(TransactionKind::Stake.mempool_class(), MempoolClass::System);
    assert_eq!(
        TransactionKind::Unstake.mempool_class(),
        MempoolClass::System
    );
    assert_eq!(
        TransactionKind::SubmitProposal.mempool_class(),
        MempoolClass::System
    );
    assert_eq!(
        TransactionKind::GovernanceVote.mempool_class(),
        MempoolClass::System
    );

    // User: everything else, including EVM.
    assert_eq!(
        TransactionKind::Transfer.mempool_class(),
        MempoolClass::User
    );
    assert_eq!(
        TransactionKind::DeployContract.mempool_class(),
        MempoolClass::User
    );
    assert_eq!(
        TransactionKind::CallContract.mempool_class(),
        MempoolClass::User
    );
    assert_eq!(
        TransactionKind::DeployToken.mempool_class(),
        MempoolClass::User
    );
    assert_eq!(
        TransactionKind::TokenTransfer.mempool_class(),
        MempoolClass::User
    );
    assert_eq!(
        TransactionKind::TokenApprove.mempool_class(),
        MempoolClass::User
    );
    assert_eq!(
        TransactionKind::TokenTransferFrom.mempool_class(),
        MempoolClass::User
    );
    assert_eq!(
        TransactionKind::DeployEvmContract.mempool_class(),
        MempoolClass::User
    );
    assert_eq!(
        TransactionKind::CallEvmContract.mempool_class(),
        MempoolClass::User
    );
}

/// Build a chain whose first allocation is a producer (so we can mine
/// blocks) and the rest are funded users with enough liquid to
/// transfer + enough staked-or-liquid to stake.
fn priority_class_chain(extra_funders: usize) -> (Blockchain, KeyPair, Vec<KeyPair>) {
    let producer = KeyPair::generate();
    let funders: Vec<KeyPair> = (0..extra_funders).map(|_| KeyPair::generate()).collect();
    let mut allocations = vec![GenesisAllocation {
        public_key: hex::encode(&producer.public_key),
        balance: 1_000_000_000_000,
        staked_balance: 100_000_000_000,
    }];
    for kp in &funders {
        allocations.push(GenesisAllocation {
            public_key: hex::encode(&kp.public_key),
            balance: 1_000_000_000_000,
            staked_balance: 0,
        });
    }
    let chain = Blockchain::from_genesis(GenesisConfig {
        chain_id: "mempool-class-test".to_string(),
        chain_name: "mempool-class-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        unstake_delay_blocks: DEFAULT_UNSTAKE_DELAY_BLOCKS,
        epoch_length: DEFAULT_EPOCH_LENGTH,
        jail_duration_blocks: DEFAULT_JAIL_DURATION_BLOCKS,
        block_gas_limit: 100_000,
        allocations,
        ..Default::default()
    })
    .unwrap();
    (chain, producer, funders)
}

#[test]
fn count_pending_by_class_tracks_both_pools() {
    let (mut chain, producer, funders) = priority_class_chain(2);
    // Mine block 1 so producer can submit (nonce machinery).
    let block = chain.create_block(&producer).unwrap();
    chain.add_block(block).unwrap();

    // 2 user txs (Transfer) + 1 system tx (Stake).
    let recipient = hash::address_bytes_from_public_key(&KeyPair::generate().public_key);
    for (i, kp) in funders.iter().enumerate() {
        let mut tx = Transaction::new(
            chain.chain_id(),
            kp.public_key.clone(),
            recipient.clone(),
            10_000,
            10,
            0,
        );
        tx.sign(kp);
        chain.add_transaction(tx).unwrap();
        assert_eq!(chain.pending_transactions.len(), i + 1);
    }
    let mut stake_tx = Transaction::stake(
        chain.chain_id(),
        funders[0].public_key.clone(),
        2_000,
        10,
        1,
    );
    stake_tx.sign(&funders[0]);
    chain.add_transaction(stake_tx).unwrap();

    let (system, user) = chain.count_pending_by_class();
    assert_eq!(system, 1, "1 Stake = 1 system");
    assert_eq!(user, 2, "2 Transfer = 2 user");
}

#[test]
fn sort_pending_puts_system_class_first() {
    let (mut chain, producer, funders) = priority_class_chain(3);
    let block = chain.create_block(&producer).unwrap();
    chain.add_block(block).unwrap();

    // Add in the order [Transfer, Stake, Transfer] — different
    // senders so the sort can freely reorder by class.
    let recipient = hash::address_bytes_from_public_key(&KeyPair::generate().public_key);

    let mut tx_user_a = Transaction::new(
        chain.chain_id(),
        funders[0].public_key.clone(),
        recipient.clone(),
        10_000,
        50,
        0,
    );
    tx_user_a.sign(&funders[0]);
    chain.add_transaction(tx_user_a).unwrap();

    // System tx with intentionally LOWER fee — class ordering must
    // override fee priority for inter-class comparisons.
    let mut tx_system = Transaction::stake(
        chain.chain_id(),
        funders[1].public_key.clone(),
        2_000,
        10,
        0,
    );
    tx_system.sign(&funders[1]);
    let system_tx_hash = tx_system.hash();
    chain.add_transaction(tx_system).unwrap();

    let mut tx_user_b = Transaction::new(
        chain.chain_id(),
        funders[2].public_key.clone(),
        recipient,
        10_000,
        50,
        0,
    );
    tx_user_b.sign(&funders[2]);
    chain.add_transaction(tx_user_b).unwrap();

    // After sort, the system tx must be at position 0 even though
    // its fee is lower than the user txs.
    assert_eq!(
        chain.pending_transactions[0].hash(),
        system_tx_hash,
        "system tx must sort to the front of the pending vec, regardless of fee"
    );
    assert_eq!(
        chain.pending_transactions[0].mempool_class(),
        MempoolClass::System
    );
    assert_eq!(
        chain.pending_transactions[1].mempool_class(),
        MempoolClass::User
    );
    assert_eq!(
        chain.pending_transactions[2].mempool_class(),
        MempoolClass::User
    );
}

#[test]
fn worst_pending_in_class_only_returns_that_class() {
    // The class-restricted eviction helper is the lynchpin of the
    // "System class never evicted by user pressure" invariant. This
    // unit test pins its behaviour: it must only return indices
    // matching the requested class, regardless of relative fees.
    let (mut chain, producer, funders) = priority_class_chain(3);
    let block = chain.create_block(&producer).unwrap();
    chain.add_block(block).unwrap();

    let recipient = hash::address_bytes_from_public_key(&KeyPair::generate().public_key);

    // System tx with a LOW fee — would be the worst candidate
    // overall if class wasn't filtered.
    let mut stake_tx =
        Transaction::stake(chain.chain_id(), funders[0].public_key.clone(), 2_000, 5, 0);
    stake_tx.sign(&funders[0]);
    let stake_hash = stake_tx.hash();
    chain.add_transaction(stake_tx).unwrap();

    // User tx with a HIGHER fee than the system tx.
    let mut tx_user = Transaction::new(
        chain.chain_id(),
        funders[1].public_key.clone(),
        recipient,
        1_000,
        50,
        0,
    );
    tx_user.sign(&funders[1]);
    let user_hash = tx_user.hash();
    chain.add_transaction(tx_user).unwrap();

    // worst-in-User returns the user tx (only user-class entry).
    let idx_user = chain
        .worst_pending_transaction_index_in_class(MempoolClass::User)
        .expect("user-class worst must exist");
    assert_eq!(chain.pending_transactions[idx_user].hash(), user_hash);
    assert_eq!(
        chain.pending_transactions[idx_user].mempool_class(),
        MempoolClass::User
    );

    // worst-in-System returns the stake tx (only system-class
    // entry) — never the user tx, even though the user has a
    // higher fee that would normally lose the "worst" race.
    let idx_system = chain
        .worst_pending_transaction_index_in_class(MempoolClass::System)
        .expect("system-class worst must exist");
    assert_eq!(chain.pending_transactions[idx_system].hash(), stake_hash);
    assert_eq!(
        chain.pending_transactions[idx_system].mempool_class(),
        MempoolClass::System
    );
}

// ───────── v6 SparseMerkleTrie state root ─────────

fn account_with_balance(balance: u64) -> AccountState {
    AccountState {
        balance,
        nonce: 0,
        staked_balance: 0,
        pending_unstakes: Vec::new(),
        validator_active_from_height: 0,
        jailed_until_height: 0,
        public_key: None,
    }
}

#[test]
fn v6_smt_root_is_deterministic_under_insertion_order() {
    let addr_a = vec![0x01; 20];
    let addr_b = vec![0x02; 20];
    let addr_c = vec![0x03; 20];

    let mut accounts_1 = HashMap::new();
    accounts_1.insert(addr_a.clone(), account_with_balance(100));
    accounts_1.insert(addr_b.clone(), account_with_balance(200));
    accounts_1.insert(addr_c.clone(), account_with_balance(300));

    let mut accounts_2 = HashMap::new();
    accounts_2.insert(addr_c, account_with_balance(300));
    accounts_2.insert(addr_a, account_with_balance(100));
    accounts_2.insert(addr_b, account_with_balance(200));

    let root_1 = Blockchain::compute_state_root_at_protocol(&accounts_1, &HashMap::new(), 6);
    let root_2 = Blockchain::compute_state_root_at_protocol(&accounts_2, &HashMap::new(), 6);
    assert_eq!(
        root_1, root_2,
        "v6 SMT root must be independent of HashMap iteration order"
    );
}

#[test]
fn v6_smt_root_differs_from_v5_merkle_root() {
    // Different commitment schemes → different bytes for non-empty
    // state. (The empty-state case is intentionally allowed to
    // differ — see the documentation on `compute_state_root_v6_smt`.)
    let mut accounts = HashMap::new();
    accounts.insert(vec![0x42; 20], account_with_balance(1_000_000));

    let v5 = Blockchain::compute_state_root_at_protocol(&accounts, &HashMap::new(), 5);
    let v6 = Blockchain::compute_state_root_at_protocol(&accounts, &HashMap::new(), 6);
    assert_ne!(v5, v6, "v5 and v6 commitments must produce different bytes");
    assert_eq!(v6.len(), 32, "v6 SMT root must be 32 bytes");
}

#[test]
fn dispatch_below_v6_uses_legacy_merkle() {
    // For any protocol version < 6 the dispatcher must produce the
    // EXACT same bytes as the legacy compute_state_root_full.
    // Without this guarantee, switching call sites to the dispatcher
    // would silently change the bytes that go on-chain — a hardfork
    // in disguise.
    let mut accounts = HashMap::new();
    accounts.insert(vec![0x05; 20], account_with_balance(500));
    accounts.insert(vec![0x06; 20], account_with_balance(600));

    let legacy = Blockchain::compute_state_root_full(&accounts, &HashMap::new());
    let dispatched_v1 = Blockchain::compute_state_root_at_protocol(&accounts, &HashMap::new(), 1);
    let dispatched_v3 = Blockchain::compute_state_root_at_protocol(&accounts, &HashMap::new(), 3);
    let dispatched_v5 = Blockchain::compute_state_root_at_protocol(&accounts, &HashMap::new(), 5);
    assert_eq!(dispatched_v1, legacy);
    assert_eq!(dispatched_v3, legacy);
    assert_eq!(dispatched_v5, legacy);
}

#[test]
fn v6_hardfork_height_is_dormant_at_baseline() {
    // The hardfork constant ships as `u64::MAX`. Make sure
    // protocol_version_at_height never returns v6 unless the
    // constant is explicitly lowered or a genesis upgrade asks for
    // it. Regression guard against an accidental constant bump.
    assert_eq!(V6_HARDFORK_HEIGHT_TESTNET, u64::MAX);
    let chain = Blockchain::new();
    assert_eq!(chain.protocol_version_at_height(0), 5);
    assert_eq!(chain.protocol_version_at_height(1), 5);
    assert_eq!(chain.protocol_version_at_height(1_000_000), 5);
    assert_eq!(chain.protocol_version_at_height(u64::MAX - 1), 5);
    // Only the absolute top, which is intentionally unreachable in
    // a real chain, would trigger v6 with the current constant.
    assert_eq!(
        chain.protocol_version_at_height(u64::MAX),
        V6_PROTOCOL_VERSION
    );
}

#[test]
fn worst_pending_in_empty_class_returns_none() {
    // If a class is empty, the helper returns None and the eviction
    // loop falls through to the other class (via the .or_else chain
    // in enforce_mempool_limits).
    let (mut chain, producer, funders) = priority_class_chain(1);
    let block = chain.create_block(&producer).unwrap();
    chain.add_block(block).unwrap();

    let recipient = hash::address_bytes_from_public_key(&KeyPair::generate().public_key);
    let mut tx_user = Transaction::new(
        chain.chain_id(),
        funders[0].public_key.clone(),
        recipient,
        1_000,
        10,
        0,
    );
    tx_user.sign(&funders[0]);
    chain.add_transaction(tx_user).unwrap();

    // No system txs in pool.
    assert!(
        chain
            .worst_pending_transaction_index_in_class(MempoolClass::System)
            .is_none()
    );
    // User-class lookup finds the lone transfer.
    assert!(
        chain
            .worst_pending_transaction_index_in_class(MempoolClass::User)
            .is_some()
    );
}

/// Post-D.3 the cursor IS the canonical block view. The Phase-B
/// dual-write coherence assertion is gone (nothing to compare
/// against). This replacement test exercises the same scenario but
/// asserts cursor self-consistency: every block added via
/// `add_block` must be readable from the cursor at its height, the
/// `block_count` must track `height + 1`, and the cursor's view of
/// the head must match the head a fresh `with_storage` reload sees.
#[test]
fn cursor_is_canonical_block_view_after_add_block() {
    let dir = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-cursor-canonical-test".to_string(),
        chain_name: "curs3d-cursor-canonical-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        epoch_length: 8,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };
    let data_dir = dir.path().to_str().unwrap();
    let mut chain = Blockchain::with_storage(data_dir, Some(&genesis)).unwrap();

    // Track the hashes we just produced so we can verify the cursor
    // (and a fresh reload) report the same chain.
    let mut expected_hashes = vec![chain.genesis_block().hash];
    for _ in 0..10 {
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block.clone()).unwrap();
        expected_hashes.push(block.hash);

        let head = chain.height();
        assert_eq!(chain.block_count(), head + 1);

        for h in 0..=head {
            let read = chain
                .block_at_height(h)
                .unwrap_or_else(|| panic!("cursor missing block at height {h}"));
            assert_eq!(
                read.hash, expected_hashes[h as usize],
                "cursor hash mismatch at height {h}"
            );
            assert_eq!(read.header.height, h);
        }
    }

    // After a fresh reload (rebuilds the cursor from redb), the chain
    // must see the same blocks. This catches any persistence bug that
    // wouldn't show up while the cursor's in-memory cache still has
    // the blocks.
    drop(chain);
    let reopened = Blockchain::with_storage(data_dir, Some(&genesis)).unwrap();
    assert_eq!(reopened.block_count(), expected_hashes.len() as u64);
    for (h, expected) in expected_hashes.iter().enumerate() {
        let read = reopened
            .block_at_height(h as u64)
            .unwrap_or_else(|| panic!("reload missing block at height {h}"));
        assert_eq!(&read.hash, expected, "reload hash mismatch at height {h}");
    }
}

/// #28 Phase E — `maybe_prune_finalized` drops history below
/// `finalized - keep_blocks`. Archival mode (None) is a no-op. With
/// `Some(K)`, finalizing block F prunes [1, F-K). Genesis (h=0)
/// always stays pinned. The cursor's storage backend (redb in this
/// test) must report the pruned heights as gone too — otherwise
/// the disk usage never shrinks.
#[test]
fn maybe_prune_finalized_drops_history_below_watermark() {
    let dir = tempfile::tempdir().unwrap();
    let validator = KeyPair::generate();
    let genesis = GenesisConfig {
        chain_id: "curs3d-prune-test".to_string(),
        chain_name: "curs3d-prune-test".to_string(),
        block_reward: DEFAULT_BLOCK_REWARD,
        minimum_stake: 1_000,
        epoch_length: 8,
        allocations: vec![GenesisAllocation {
            public_key: hex::encode(&validator.public_key),
            balance: 1_000_000_000,
            staked_balance: 5_000,
        }],
        ..Default::default()
    };
    let data_dir = dir.path().to_str().unwrap();
    let mut chain = Blockchain::with_storage(data_dir, Some(&genesis)).unwrap();

    // Build a chain of 20 blocks so we have a meaningful prune target.
    for _ in 0..20 {
        let block = chain.create_block(&validator).unwrap();
        chain.add_block(block).unwrap();
    }
    assert_eq!(chain.height(), 20);
    assert_eq!(chain.chain_base_height(), 0, "archival mode = no prune");

    // Archival mode: no prune even with a finalization event.
    let removed = chain.maybe_prune_finalized(15);
    assert_eq!(removed, 0);
    assert_eq!(chain.chain_base_height(), 0);
    assert!(chain.block_at_height(5).is_some(), "no prune in archival");

    // Enable pruning with a 5-block retention window. Finalize at
    // height 15 → keep heights [10, 15], drop heights [1, 9].
    chain.set_prune_keep_blocks(Some(5));
    assert_eq!(chain.prune_keep_blocks(), Some(5));
    let removed = chain.maybe_prune_finalized(15);
    assert!(removed > 0, "should have pruned some history");
    assert_eq!(chain.chain_base_height(), 10);

    // Pruned heights return None; retained heights still resolve.
    for h in 1..10 {
        assert!(
            chain.block_at_height(h).is_none(),
            "height {h} should be pruned"
        );
    }
    for h in 10..=20 {
        assert!(
            chain.block_at_height(h).is_some(),
            "height {h} must still resolve"
        );
    }
    // Genesis is always preserved (pinned by cursor).
    assert!(chain.block_at_height(0).is_some());

    // A second prune at the same finalized height is a no-op.
    let removed = chain.maybe_prune_finalized(15);
    assert_eq!(removed, 0);
    assert_eq!(chain.chain_base_height(), 10);

    // Disable pruning again: future finalizations don't extend the
    // prune watermark.
    chain.set_prune_keep_blocks(None);
    let removed = chain.maybe_prune_finalized(20);
    assert_eq!(removed, 0);
    assert_eq!(chain.chain_base_height(), 10, "watermark unchanged");
}
