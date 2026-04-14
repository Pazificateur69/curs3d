mod api;
mod consensus;
mod core;
mod crypto;
mod governance;
mod network;
mod rpc;
mod storage;
mod token;
mod vm;
mod wallet;

use std::sync::Arc;
use std::{collections::BTreeMap, fs, path::PathBuf};

use clap::{Parser, Subcommand};
use libp2p::{Multiaddr, identity};
use tokio::sync::Mutex;
use tracing::info;

use crate::core::chain::{
    AccountState, Blockchain, DEFAULT_BLOCK_REWARD, DEFAULT_EPOCH_LENGTH, DEFAULT_MIN_STAKE,
    GenesisAllocation, GenesisConfig,
};
use crate::rpc::{RpcRequest, RpcResponse};

const DEFAULT_DATA_DIR: &str = "curs3d_data";
const DEFAULT_RPC_ADDR: &str = "127.0.0.1:9545";
const MICROTOKENS_PER_CUR: u64 = 1_000_000;

#[derive(Parser)]
#[command(name = "curs3d")]
#[command(about = "CURS3D — Quantum-Resistant Blockchain Node", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Node {
        #[arg(short, long, default_value_t = 4337)]
        port: u16,
        #[arg(short, long, default_value = DEFAULT_DATA_DIR)]
        data_dir: String,
        #[arg(long)]
        validator_wallet: Option<String>,
        #[arg(long)]
        validator_password_file: Option<String>,
        #[arg(long = "bootnode")]
        bootnodes: Vec<String>,
        #[arg(long = "public-addr")]
        public_addrs: Vec<String>,
        #[arg(long, default_value = DEFAULT_RPC_ADDR)]
        rpc_addr: String,
        #[arg(long)]
        genesis_config: Option<String>,
    },
    Wallet {
        #[arg(short, long, default_value = "wallet.json")]
        output: String,
        #[arg(long)]
        password_file: Option<String>,
    },
    Info {
        #[arg(short, long, default_value = "wallet.json")]
        wallet: String,
        #[arg(long)]
        password_file: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Send {
        #[arg(short, long, default_value = "wallet.json")]
        wallet: String,
        #[arg(long)]
        password_file: Option<String>,
        #[arg(short, long)]
        to: String,
        #[arg(short, long)]
        amount: u64,
        #[arg(short, long, default_value_t = 1000)]
        fee: u64,
        #[arg(long, default_value = DEFAULT_DATA_DIR)]
        data_dir: String,
        #[arg(long, default_value = DEFAULT_RPC_ADDR)]
        rpc_addr: String,
    },
    Status {
        #[arg(short, long, default_value = DEFAULT_DATA_DIR)]
        data_dir: String,
        #[arg(long)]
        rpc_addr: Option<String>,
    },
    Stake {
        #[arg(short, long, default_value = "wallet.json")]
        wallet: String,
        #[arg(long)]
        password_file: Option<String>,
        #[arg(short, long)]
        amount: u64,
        #[arg(short, long, default_value_t = 1000)]
        fee: u64,
        #[arg(long, default_value = DEFAULT_DATA_DIR)]
        data_dir: String,
        #[arg(long, default_value = DEFAULT_RPC_ADDR)]
        rpc_addr: String,
    },
    Genesis {
        #[arg(long, default_value = "deploy/genesis.public-testnet.json")]
        output: String,
        #[arg(long, default_value = "curs3d-public-testnet")]
        chain_id: String,
        #[arg(long, default_value = "CURS3D Public Testnet")]
        chain_name: String,
        #[arg(long)]
        validator_wallet: String,
        #[arg(long)]
        validator_password_file: Option<String>,
        #[arg(long, default_value_t = 1_500_000)]
        validator_balance_cur: u64,
        #[arg(long, default_value_t = 50_000)]
        validator_stake_cur: u64,
        #[arg(long)]
        faucet_wallet: Option<String>,
        #[arg(long)]
        faucet_password_file: Option<String>,
        #[arg(long, default_value_t = 2_000_000)]
        faucet_balance_cur: u64,
        #[arg(long, default_value_t = DEFAULT_BLOCK_REWARD / MICROTOKENS_PER_CUR)]
        block_reward_cur: u64,
        #[arg(long, default_value_t = DEFAULT_MIN_STAKE / MICROTOKENS_PER_CUR)]
        minimum_stake_cur: u64,
        #[arg(long, default_value_t = DEFAULT_EPOCH_LENGTH)]
        epoch_length: u64,
    },
    BootnodeAddress {
        #[arg(short, long, default_value = DEFAULT_DATA_DIR)]
        data_dir: String,
        #[arg(long = "public-addr")]
        public_addrs: Vec<String>,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::Node {
            port,
            data_dir,
            validator_wallet,
            validator_password_file,
            bootnodes,
            public_addrs,
            rpc_addr,
            genesis_config,
        } => {
            run_node(
                port,
                &data_dir,
                validator_wallet.as_deref(),
                validator_password_file.as_deref(),
                &bootnodes,
                &public_addrs,
                &rpc_addr,
                genesis_config.as_deref(),
            )
            .await
        }
        Commands::Wallet {
            output,
            password_file,
        } => create_wallet(&output, password_file.as_deref()),
        Commands::Info {
            wallet: path,
            password_file,
            json,
        } => show_wallet_info(&path, password_file.as_deref(), json),
        Commands::Send {
            wallet: path,
            password_file,
            to,
            amount,
            fee,
            data_dir,
            rpc_addr,
        } => {
            send_tokens(
                &path,
                password_file.as_deref(),
                &to,
                amount,
                fee,
                &data_dir,
                &rpc_addr,
            )
            .await
        }
        Commands::Status { data_dir, rpc_addr } => {
            show_status(&data_dir, rpc_addr.as_deref()).await
        }
        Commands::Stake {
            wallet: path,
            password_file,
            amount,
            fee,
            data_dir,
            rpc_addr,
        } => {
            stake_tokens(
                &path,
                password_file.as_deref(),
                amount,
                fee,
                &data_dir,
                &rpc_addr,
            )
            .await
        }
        Commands::Genesis {
            output,
            chain_id,
            chain_name,
            validator_wallet,
            validator_password_file,
            validator_balance_cur,
            validator_stake_cur,
            faucet_wallet,
            faucet_password_file,
            faucet_balance_cur,
            block_reward_cur,
            minimum_stake_cur,
            epoch_length,
        } => generate_genesis(
            &output,
            &chain_id,
            &chain_name,
            &validator_wallet,
            validator_password_file.as_deref(),
            validator_balance_cur,
            validator_stake_cur,
            faucet_wallet.as_deref(),
            faucet_password_file.as_deref(),
            faucet_balance_cur,
            block_reward_cur,
            minimum_stake_cur,
            epoch_length,
        ),
        Commands::BootnodeAddress {
            data_dir,
            public_addrs,
        } => show_bootnode_addresses(&data_dir, &public_addrs),
    }
}

fn create_wallet(path: &str, password_file: Option<&str>) {
    if wallet::Wallet::exists(path) {
        println!("Wallet already exists at {}", path);
        return;
    }

    let password = resolve_password(
        password_file,
        "CURS3D_WALLET_PASSWORD_FILE",
        "CURS3D_WALLET_PASSWORD",
        Some(("Create wallet password: ", true)),
    );

    let w = wallet::Wallet::new();
    match w.save_encrypted(path, &password) {
        Ok(()) => {
            println!();
            println!("=== CURS3D Wallet Created ===");
            println!("Address: {}", w.address);
            println!("Saved to: {} (AES-256-GCM encrypted)", path);
            println!();
            println!("IMPORTANT: Remember your password. There is no recovery.");
            println!("Keys: CRYSTALS-Dilithium Level 5");
        }
        Err(e) => eprintln!("Failed to save wallet: {}", e),
    }
}

fn show_wallet_info(path: &str, password_file: Option<&str>, json: bool) {
    let password = resolve_password(
        password_file,
        "CURS3D_WALLET_PASSWORD_FILE",
        "CURS3D_WALLET_PASSWORD",
        Some(("Enter wallet password: ", false)),
    );
    match wallet::Wallet::load_auto(path, &password) {
        Ok(w) => {
            if json {
                let payload = serde_json::json!({
                    "address": w.address,
                    "public_key": w.keypair.public_key_hex(),
                    "algorithm": "CRYSTALS-Dilithium (Level 5)",
                    "encryption": "AES-256-GCM + Argon2"
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .expect("wallet info json must serialize")
                );
            } else {
                println!("=== CURS3D Wallet ===");
                println!("Address:    {}", w.address);
                println!("Public Key: {}", w.keypair.public_key_hex());
                println!("Algorithm:  CRYSTALS-Dilithium (Level 5)");
                println!("Encryption: AES-256-GCM + Argon2");
            }
        }
        Err(wallet::WalletError::WrongPassword) => eprintln!("Error: Wrong password."),
        Err(e) => eprintln!("Failed to load wallet: {}", e),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_node(
    port: u16,
    data_dir: &str,
    validator_wallet: Option<&str>,
    validator_password_file: Option<&str>,
    bootnodes: &[String],
    public_addrs: &[String],
    rpc_addr: &str,
    genesis_config_path: Option<&str>,
) {
    println!(
        r#"
   ██████╗██╗   ██╗██████╗ ███████╗██████╗ ██████╗
  ██╔════╝██║   ██║██╔══██╗██╔════╝╚════██╗██╔══██╗
  ██║     ██║   ██║██████╔╝███████╗ █████╔╝██║  ██║
  ██║     ██║   ██║██╔══██╗╚════██║ ╚═══██╗██║  ██║
  ╚██████╗╚██████╔╝██║  ██║███████║██████╔╝██████╔╝
   ╚═════╝ ╚═════╝ ╚═╝  ╚═╝╚══════╝╚═════╝ ╚═════╝
                 Quantum-Resistant Blockchain
    "#
    );

    println!("Starting CURS3D node on port {}...", port);
    println!("Data directory: {}", data_dir);
    if let Err(err) = fs::create_dir_all(data_dir) {
        eprintln!("Failed to create data directory: {}", err);
        return;
    }

    let genesis_config = match load_genesis_config(genesis_config_path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("Failed to load genesis config: {}", err);
            return;
        }
    };

    let chain = match Blockchain::with_storage(data_dir, genesis_config.as_ref()) {
        Ok(chain) => chain,
        Err(e) => {
            eprintln!("Failed to initialize blockchain storage: {}", e);
            eprintln!(
                "Node startup aborted to avoid joining the network with divergent in-memory state."
            );
            return;
        }
    };

    let chain_height = chain.height();
    let latest_hash = chain.latest_block().hash_hex();
    let network_topic = network::topic_name(
        chain.chain_id(),
        chain.protocol_version_at_height(chain.height()),
    );
    info!(
        "Blockchain loaded. Chain: {}, Height: {}, Latest: {}",
        chain.genesis_config.chain_name,
        chain_height,
        &latest_hash[..16]
    );

    let chain = Arc::new(Mutex::new(chain));

    let validator_key = if let Some(wallet_path) = validator_wallet {
        let password = resolve_password(
            validator_password_file,
            "CURS3D_VALIDATOR_PASSWORD_FILE",
            "CURS3D_VALIDATOR_PASSWORD",
            Some(("Enter validator wallet password: ", false)),
        );
        match wallet::Wallet::load_auto(wallet_path, &password) {
            Ok(w) => {
                println!("Validator wallet loaded: {}", w.address);
                Some(w.keypair)
            }
            Err(wallet::WalletError::WrongPassword) => {
                eprintln!("Wrong password for validator wallet. Running without block production.");
                None
            }
            Err(e) => {
                eprintln!(
                    "Failed to load validator wallet: {}. Running without block production.",
                    e
                );
                None
            }
        }
    } else {
        println!("No validator wallet specified. Running as relay node.");
        None
    };

    let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(100);
    let (event_tx, _event_rx) = tokio::sync::broadcast::channel::<String>(256);

    // TCP RPC (for CLI)
    let rpc_chain = Arc::clone(&chain);
    let rpc_addr_owned = rpc_addr.to_string();
    let rpc_outbound_tx = outbound_tx.clone();
    let rpc_task =
        tokio::spawn(async move { rpc::serve(&rpc_addr_owned, rpc_chain, rpc_outbound_tx).await });

    // HTTP API (for browser/explorer)
    let http_chain = Arc::clone(&chain);
    let http_event_tx = event_tx.clone();
    let http_outbound_tx = outbound_tx.clone();
    let http_addr = rpc_addr.replace("9545", "8080");
    let http_task = tokio::spawn(async move {
        if let Err(e) =
            api::serve_http(&http_addr, http_chain, http_event_tx, http_outbound_tx).await
        {
            tracing::error!("HTTP API error: {}", e);
        }
    });

    let p2p_identity = match load_or_create_p2p_identity(data_dir) {
        Ok(keypair) => keypair,
        Err(err) => {
            eprintln!("Failed to load P2P identity: {}", err);
            return;
        }
    };
    let public_multiaddrs = match parse_multiaddrs(public_addrs) {
        Ok(addrs) => addrs,
        Err(err) => {
            eprintln!("Invalid public address: {}", err);
            return;
        }
    };
    let bootnode_addresses = build_bootnode_addresses(&p2p_identity, &public_multiaddrs);
    if let Err(err) = persist_bootnode_addresses(data_dir, &bootnode_addresses) {
        eprintln!("Failed to persist bootnode addresses: {}", err);
        return;
    }

    match network::NetworkNode::new(
        port,
        bootnodes,
        &network_topic,
        p2p_identity,
        &public_multiaddrs,
    )
    .await
    {
        Ok(mut node) => {
            let (active_validators, pending_txs, chain_id, chain_name, genesis_hash) = {
                let chain_lock = chain.lock().await;
                (
                    chain_lock.active_validator_count(),
                    chain_lock.pending_transactions.len(),
                    chain_lock.chain_id().to_string(),
                    chain_lock.genesis_config.chain_name.clone(),
                    hex::encode(chain_lock.genesis_hash()),
                )
            };

            println!();
            println!("Chain ID: {}", chain_id);
            println!("Chain: {}", chain_name);
            println!("Genesis: {}", genesis_hash);
            println!("Network topic: {}", network_topic);
            println!("Node PeerId: {}", node.peer_id);
            println!("Listening on port {}", port);
            println!("RPC listening on {}", rpc_addr);
            println!("HTTP API on http://{}", rpc_addr.replace("9545", "8080"));
            println!("Chain height: {}", chain_height);
            println!("Active validators: {}", active_validators);
            println!("Pending txs: {}", pending_txs);
            println!("Bootnodes: {}", bootnodes.len());
            if bootnode_addresses.is_empty() {
                println!(
                    "Bootnode publish addresses: none configured (use --public-addr to publish WAN bootstrap addresses)"
                );
            } else {
                println!("Bootnode publish addresses:");
                for address in &bootnode_addresses {
                    println!("  {}", address);
                }
            }
            if let Some(ref keypair) = validator_key {
                let stake = {
                    let chain_lock = chain.lock().await;
                    let address = wallet::Wallet::derive_address_bytes(&keypair.public_key);
                    chain_lock.get_staked_balance(&address)
                };
                println!(
                    "Producer wallet: {} (staked: {} CURS3D)",
                    wallet::Wallet::derive_address(&keypair.public_key),
                    stake / 1_000_000
                );
            } else {
                println!("Mode: RELAY");
            }
            println!();
            println!("Press Ctrl+C to stop the node.");

            tokio::select! {
                _ = node.run_with_chain(chain, outbound_rx, validator_key, Some(event_tx.clone())) => {}
                rpc_result = rpc_task => {
                    match rpc_result {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => eprintln!("RPC server stopped: {}", err),
                        Err(err) => eprintln!("RPC task failed: {}", err),
                    }
                }
                _ = http_task => {}
                _ = tokio::signal::ctrl_c() => {
                    println!("\nShutting down gracefully...");
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to start network node: {}", e);
            eprintln!("Running in offline mode...");

            let chain_lock = chain.lock().await;
            println!("Blockchain running in offline mode.");
            println!("Height: {}", chain_lock.height());
            println!("Latest: {}", chain_lock.latest_block().hash_hex());
            drop(chain_lock);

            tokio::signal::ctrl_c()
                .await
                .expect("failed to listen for ctrl-c");
            println!("\nShutting down...");
        }
    }
}

async fn send_tokens(
    wallet_path: &str,
    password_file: Option<&str>,
    to: &str,
    amount: u64,
    fee: u64,
    data_dir: &str,
    rpc_addr: &str,
) {
    let password = resolve_password(
        password_file,
        "CURS3D_WALLET_PASSWORD_FILE",
        "CURS3D_WALLET_PASSWORD",
        Some(("Enter wallet password: ", false)),
    );
    let w = match wallet::Wallet::load_auto(wallet_path, &password) {
        Ok(w) => w,
        Err(wallet::WalletError::WrongPassword) => {
            eprintln!("Error: Wrong password.");
            return;
        }
        Err(e) => {
            eprintln!("Failed to load wallet: {}", e);
            return;
        }
    };

    let sender_address = wallet::Wallet::derive_address_bytes(&w.keypair.public_key);
    let account_state = match fetch_account_state(sender_address.clone(), data_dir, rpc_addr).await
    {
        Ok(state) => state,
        Err(err) => {
            eprintln!("Failed to resolve account state: {}", err);
            return;
        }
    };

    let amount_micro = amount.saturating_mul(1_000_000);
    let total_needed = amount_micro.saturating_add(fee);
    if account_state.balance < total_needed {
        eprintln!(
            "Insufficient balance: have {} CURS3D, need {} CURS3D + {} microtoken fee",
            account_state.balance / 1_000_000,
            amount,
            fee
        );
        return;
    }

    let to_bytes = match decode_address(to) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("{}", err);
            return;
        }
    };

    let mut tx = crate::core::transaction::Transaction::new(
        &resolve_chain_id(data_dir, rpc_addr)
            .await
            .unwrap_or_else(|_| "curs3d-devnet".to_string()),
        w.keypair.public_key.clone(),
        to_bytes,
        amount_micro,
        fee,
        account_state.nonce,
    );
    tx.sign(&w.keypair);

    match submit_transaction(tx.clone(), data_dir, rpc_addr).await {
        Ok(mode) => {
            println!("=== CURS3D Transaction Submitted ===");
            println!("From:   {}", w.address);
            println!("To:     {}", to);
            println!("Amount: {} CURS3D", amount);
            println!("Fee:    {} microtokens", fee);
            println!("Nonce:  {}", account_state.nonce);
            println!("TxHash: {}", tx.hash_hex());
            println!("Route:  {}", mode);
        }
        Err(e) => eprintln!("Failed to submit transaction: {}", e),
    }
}

async fn show_status(data_dir: &str, rpc_addr: Option<&str>) {
    if let Some(addr) = rpc_addr {
        match rpc::send_request(addr, &RpcRequest::GetStatus).await {
            Ok(RpcResponse::Status { status }) => {
                println!("=== CURS3D Node Status ===");
                println!("Chain ID:          {}", status.chain_id);
                println!("Chain:             {}", status.chain_name);
                println!("Epoch:             {}", status.epoch);
                println!("Epoch Start:       {}", status.epoch_start_height);
                println!("Height:            {}", status.height);
                println!("Finalized:         {}", status.finalized_height);
                println!("Latest Hash:       {}", status.latest_hash);
                println!("Genesis Hash:      {}", status.genesis_hash);
                println!("Pending Txs:       {}", status.pending_transactions);
                println!("Active Validators: {}", status.active_validators);
                println!("Source:            RPC {}", addr);
                return;
            }
            Ok(RpcResponse::Error { message }) => {
                eprintln!("RPC status error: {}", message);
            }
            Ok(_) => {
                eprintln!("RPC status error: unexpected response");
            }
            Err(err) => {
                eprintln!("RPC status unavailable: {}", err);
            }
        }
    }

    let chain = match Blockchain::with_storage(data_dir, None) {
        Ok(c) => c,
        Err(_) => {
            println!("No blockchain data found. Run a node first.");
            return;
        }
    };

    println!("=== CURS3D Blockchain Status ===");
    println!("Chain ID:          {}", chain.chain_id());
    println!("Chain:             {}", chain.genesis_config.chain_name);
    println!("Epoch:             {}", chain.current_epoch());
    println!("Epoch Start:       {}", chain.current_epoch_start_height());
    println!("Height:            {}", chain.height());
    println!("Latest Hash:       {}", chain.latest_block().hash_hex());
    println!("Genesis Hash:      {}", hex::encode(chain.genesis_hash()));
    println!(
        "Block Reward:      {} CURS3D",
        chain.block_reward / 1_000_000
    );
    println!(
        "Minimum Stake:     {} CURS3D",
        chain.minimum_stake / 1_000_000
    );
    println!("Consensus:         Proof of Stake");
    println!("Crypto:            CRYSTALS-Dilithium + SHA3-256");
    println!("Storage:           sled");
    println!("Data Dir:          {}", data_dir);
    println!("Active Validators: {}", chain.active_validator_count());
    println!("Pending Txs:       {}", chain.pending_transactions.len());

    let total_accounts = chain.accounts.len();
    let circulating_supply: u64 = chain.accounts.values().map(|a| a.balance).sum();
    let total_staked: u64 = chain.accounts.values().map(|a| a.staked_balance).sum();
    println!("Accounts:          {}", total_accounts);
    println!(
        "Circulating:       {} CURS3D",
        circulating_supply / 1_000_000
    );
    println!("Staked:            {} CURS3D", total_staked / 1_000_000);
    println!(
        "Total Supply:      {} CURS3D",
        (circulating_supply + total_staked) / 1_000_000
    );
}

async fn stake_tokens(
    wallet_path: &str,
    password_file: Option<&str>,
    amount: u64,
    fee: u64,
    data_dir: &str,
    rpc_addr: &str,
) {
    let password = resolve_password(
        password_file,
        "CURS3D_WALLET_PASSWORD_FILE",
        "CURS3D_WALLET_PASSWORD",
        Some(("Enter wallet password: ", false)),
    );
    let w = match wallet::Wallet::load_auto(wallet_path, &password) {
        Ok(w) => w,
        Err(wallet::WalletError::WrongPassword) => {
            eprintln!("Error: Wrong password.");
            return;
        }
        Err(e) => {
            eprintln!("Failed to load wallet: {}", e);
            return;
        }
    };

    let sender_address = wallet::Wallet::derive_address_bytes(&w.keypair.public_key);
    let account_state = match fetch_account_state(sender_address.clone(), data_dir, rpc_addr).await
    {
        Ok(state) => state,
        Err(err) => {
            eprintln!("Failed to resolve account state: {}", err);
            return;
        }
    };

    let stake_micro = amount.saturating_mul(1_000_000);
    let needed = stake_micro.saturating_add(fee);
    if account_state.balance < needed {
        eprintln!(
            "Insufficient balance to stake: have {} CURS3D, need {} CURS3D + {} microtoken fee",
            account_state.balance / 1_000_000,
            amount,
            fee
        );
        return;
    }

    let mut tx = crate::core::transaction::Transaction::stake(
        &resolve_chain_id(data_dir, rpc_addr)
            .await
            .unwrap_or_else(|_| "curs3d-devnet".to_string()),
        w.keypair.public_key.clone(),
        stake_micro,
        fee,
        account_state.nonce,
    );
    tx.sign(&w.keypair);

    match submit_transaction(tx.clone(), data_dir, rpc_addr).await {
        Ok(mode) => {
            println!("=== CURS3D Stake Submitted ===");
            println!("Validator: {}", w.address);
            println!("Stake:     {} CURS3D", amount);
            println!("Fee:       {} microtokens", fee);
            println!("Nonce:     {}", account_state.nonce);
            println!("TxHash:    {}", tx.hash_hex());
            println!("Route:     {}", mode);
            println!();
            println!(
                "Validator becomes active on-chain after inclusion and once total stake reaches at least {} CURS3D.",
                DEFAULT_MIN_STAKE / 1_000_000
            );
        }
        Err(e) => eprintln!("Failed to submit stake transaction: {}", e),
    }
}

async fn fetch_account_state(
    address: Vec<u8>,
    data_dir: &str,
    rpc_addr: &str,
) -> Result<AccountState, String> {
    match rpc::send_request(
        rpc_addr,
        &RpcRequest::GetAccount {
            address: address.clone(),
        },
    )
    .await
    {
        Ok(RpcResponse::Account { state }) => Ok(state),
        Ok(RpcResponse::Error { message }) => Err(message),
        Ok(_) => Err("unexpected RPC response".to_string()),
        Err(_) => {
            let chain = Blockchain::with_storage(data_dir, None).map_err(|e| e.to_string())?;
            Ok(chain.get_account(&address))
        }
    }
}

async fn submit_transaction(
    tx: crate::core::transaction::Transaction,
    data_dir: &str,
    rpc_addr: &str,
) -> Result<String, String> {
    match rpc::send_request(
        rpc_addr,
        &RpcRequest::SubmitTransaction {
            transaction: tx.clone(),
        },
    )
    .await
    {
        Ok(RpcResponse::Submitted { .. }) => Ok(format!("rpc {}", rpc_addr)),
        Ok(RpcResponse::Error { message }) => Err(message),
        Ok(_) => Err("unexpected RPC response".to_string()),
        Err(_) => {
            let mut chain = Blockchain::with_storage(data_dir, None).map_err(|e| e.to_string())?;
            chain.add_transaction(tx).map_err(|e| e.to_string())?;
            Ok(format!("local {}", data_dir))
        }
    }
}

async fn resolve_chain_id(data_dir: &str, rpc_addr: &str) -> Result<String, String> {
    match rpc::send_request(rpc_addr, &RpcRequest::GetStatus).await {
        Ok(RpcResponse::Status { status }) => Ok(status.chain_id),
        Ok(RpcResponse::Error { message }) => Err(message),
        Ok(_) => Err("unexpected RPC response".to_string()),
        Err(_) => {
            let chain = Blockchain::with_storage(data_dir, None).map_err(|e| e.to_string())?;
            Ok(chain.chain_id().to_string())
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_genesis(
    output: &str,
    chain_id: &str,
    chain_name: &str,
    validator_wallet: &str,
    validator_password_file: Option<&str>,
    validator_balance_cur: u64,
    validator_stake_cur: u64,
    faucet_wallet: Option<&str>,
    faucet_password_file: Option<&str>,
    faucet_balance_cur: u64,
    block_reward_cur: u64,
    minimum_stake_cur: u64,
    epoch_length: u64,
) {
    let validator_password = resolve_password(
        validator_password_file,
        "CURS3D_VALIDATOR_PASSWORD_FILE",
        "CURS3D_VALIDATOR_PASSWORD",
        Some(("Enter validator wallet password: ", false)),
    );
    let validator = match wallet::Wallet::load_auto(validator_wallet, &validator_password) {
        Ok(wallet) => wallet,
        Err(err) => {
            eprintln!("Failed to load validator wallet: {}", err);
            return;
        }
    };

    let mut allocations = BTreeMap::<String, (u64, u64)>::new();
    allocations.insert(
        validator.keypair.public_key_hex(),
        (
            validator_balance_cur.saturating_mul(MICROTOKENS_PER_CUR),
            validator_stake_cur.saturating_mul(MICROTOKENS_PER_CUR),
        ),
    );

    let faucet_summary = if let Some(path) = faucet_wallet {
        let faucet_password = resolve_password(
            faucet_password_file,
            "CURS3D_FAUCET_PASSWORD_FILE",
            "CURS3D_FAUCET_PASSWORD",
            Some(("Enter faucet wallet password: ", false)),
        );
        let faucet = match wallet::Wallet::load_auto(path, &faucet_password) {
            Ok(wallet) => wallet,
            Err(err) => {
                eprintln!("Failed to load faucet wallet: {}", err);
                return;
            }
        };
        let entry = allocations
            .entry(faucet.keypair.public_key_hex())
            .or_insert((0, 0));
        entry.0 = entry
            .0
            .saturating_add(faucet_balance_cur.saturating_mul(MICROTOKENS_PER_CUR));
        Some((faucet.address, path.to_string()))
    } else {
        None
    };

    let mut genesis = GenesisConfig {
        chain_id: chain_id.to_string(),
        chain_name: chain_name.to_string(),
        block_reward: block_reward_cur.saturating_mul(MICROTOKENS_PER_CUR),
        minimum_stake: minimum_stake_cur.saturating_mul(MICROTOKENS_PER_CUR),
        epoch_length,
        ..GenesisConfig::default()
    };
    genesis.allocations = allocations
        .into_iter()
        .map(
            |(public_key, (balance, staked_balance))| GenesisAllocation {
                public_key,
                balance,
                staked_balance,
            },
        )
        .collect();

    let output_path = PathBuf::from(output);
    if let Some(parent) = output_path.parent()
        && let Err(err) = fs::create_dir_all(parent)
    {
        eprintln!("Failed to create genesis directory: {}", err);
        return;
    }
    let raw = match serde_json::to_string_pretty(&genesis) {
        Ok(raw) => raw,
        Err(err) => {
            eprintln!("Failed to serialize genesis config: {}", err);
            return;
        }
    };
    if let Err(err) = fs::write(&output_path, raw) {
        eprintln!("Failed to write genesis config: {}", err);
        return;
    }

    println!("=== CURS3D Public Testnet Genesis ===");
    println!("Output:              {}", output_path.display());
    println!("Chain ID:            {}", genesis.chain_id);
    println!("Chain Name:          {}", genesis.chain_name);
    println!("Validator Wallet:    {}", validator_wallet);
    println!("Validator Address:   {}", validator.address);
    println!("Validator Stake:     {} CURS3D", validator_stake_cur);
    println!("Validator Balance:   {} CURS3D", validator_balance_cur);
    if let Some((faucet_address, faucet_path)) = faucet_summary {
        println!("Faucet Wallet:       {}", faucet_path);
        println!("Faucet Address:      {}", faucet_address);
        println!("Faucet Allocation:   {} CURS3D", faucet_balance_cur);
    }
    println!("Minimum Stake:       {} CURS3D", minimum_stake_cur);
    println!("Block Reward:        {} CURS3D", block_reward_cur);
    println!("Epoch Length:        {}", epoch_length);
    println!();
    println!(
        "Next step: publish {} and start the bootstrap validator with this file.",
        output_path.display()
    );
}

fn show_bootnode_addresses(data_dir: &str, public_addrs: &[String]) {
    if let Err(err) = fs::create_dir_all(data_dir) {
        eprintln!("Failed to create data directory: {}", err);
        return;
    }
    let identity = match load_or_create_p2p_identity(data_dir) {
        Ok(identity) => identity,
        Err(err) => {
            eprintln!("Failed to load P2P identity: {}", err);
            return;
        }
    };
    let multiaddrs = match parse_multiaddrs(public_addrs) {
        Ok(addrs) => addrs,
        Err(err) => {
            eprintln!("Invalid public address: {}", err);
            return;
        }
    };
    let addresses = build_bootnode_addresses(&identity, &multiaddrs);
    if let Err(err) = persist_bootnode_addresses(data_dir, &addresses) {
        eprintln!("Failed to persist bootnode addresses: {}", err);
        return;
    }

    println!("=== CURS3D Bootnode Addresses ===");
    println!("PeerId: {}", identity.public().to_peer_id());
    if addresses.is_empty() {
        println!(
            "No public addresses supplied. Pass --public-addr /dns4/node.example.com/tcp/4337"
        );
        return;
    }
    for address in addresses {
        println!("{}", address);
    }
}

fn load_genesis_config(path: Option<&str>) -> Result<Option<GenesisConfig>, String> {
    let Some(path) = path else {
        return Ok(None);
    };

    let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let config = serde_json::from_str::<GenesisConfig>(&raw).map_err(|e| e.to_string())?;
    Ok(Some(config))
}

fn decode_address(value: &str) -> Result<Vec<u8>, String> {
    let raw = if let Some(stripped) = value.strip_prefix("CUR") {
        stripped
    } else {
        value
    };

    let bytes = hex::decode(raw).map_err(|_| "Invalid recipient address".to_string())?;
    if bytes.len() != crate::crypto::hash::ADDRESS_LEN {
        return Err(format!(
            "Invalid recipient address length: expected {} bytes",
            crate::crypto::hash::ADDRESS_LEN
        ));
    }
    Ok(bytes)
}

fn parse_multiaddrs(values: &[String]) -> Result<Vec<Multiaddr>, String> {
    values
        .iter()
        .map(|value| value.parse::<Multiaddr>().map_err(|err| err.to_string()))
        .collect()
}

fn p2p_identity_path(data_dir: &str) -> PathBuf {
    PathBuf::from(data_dir).join("p2p_identity.pb")
}

fn bootnode_addresses_path(data_dir: &str) -> PathBuf {
    PathBuf::from(data_dir).join("bootnode.addrs")
}

fn load_or_create_p2p_identity(data_dir: &str) -> Result<identity::Keypair, String> {
    let path = p2p_identity_path(data_dir);
    if path.exists() {
        let bytes = fs::read(&path).map_err(|err| err.to_string())?;
        return identity::Keypair::from_protobuf_encoding(&bytes).map_err(|err| err.to_string());
    }

    let keypair = identity::Keypair::generate_ed25519();
    let encoded = keypair
        .to_protobuf_encoding()
        .map_err(|err| err.to_string())?;
    fs::write(&path, encoded).map_err(|err| err.to_string())?;
    Ok(keypair)
}

fn build_bootnode_addresses(
    keypair: &identity::Keypair,
    public_addrs: &[Multiaddr],
) -> Vec<String> {
    let peer_id = keypair.public().to_peer_id();
    public_addrs
        .iter()
        .map(|addr| format!("{}/p2p/{}", addr, peer_id))
        .collect()
}

fn persist_bootnode_addresses(data_dir: &str, addresses: &[String]) -> Result<(), String> {
    let path = bootnode_addresses_path(data_dir);
    let contents = if addresses.is_empty() {
        String::new()
    } else {
        format!("{}\n", addresses.join("\n"))
    };
    fs::write(path, contents).map_err(|err| err.to_string())
}

fn resolve_password(
    explicit_file: Option<&str>,
    env_file_var: &str,
    env_value_var: &str,
    prompt: Option<(&str, bool)>,
) -> String {
    if let Some(path) = explicit_file {
        return read_password_file(path);
    }

    if let Ok(path) = std::env::var(env_file_var)
        && !path.trim().is_empty()
    {
        return read_password_file(&path);
    }

    if let Ok(value) = std::env::var(env_value_var)
        && !value.is_empty()
    {
        return value;
    }

    match prompt {
        Some((message, true)) => prompt_password_create_with_prompt(message),
        Some((message, false)) => prompt_password(message),
        None => {
            eprintln!(
                "Missing password. Provide --password-file, {} or {}.",
                env_file_var, env_value_var
            );
            std::process::exit(1);
        }
    }
}

fn read_password_file(path: &str) -> String {
    let secret = fs::read_to_string(path).unwrap_or_else(|err| {
        eprintln!("Failed to read password file {}: {}", path, err);
        std::process::exit(1);
    });
    let trimmed = secret.trim().to_string();
    if trimmed.is_empty() {
        eprintln!("Password file {} is empty.", path);
        std::process::exit(1);
    }
    trimmed
}

fn prompt_password(prompt: &str) -> String {
    rpassword::prompt_password(prompt).unwrap_or_else(|_| {
        eprintln!("Failed to read password");
        std::process::exit(1);
    })
}

fn prompt_password_create_with_prompt(prompt: &str) -> String {
    let pass1 = prompt_password(prompt);
    let pass2 = prompt_password("Confirm password: ");

    if pass1 != pass2 {
        eprintln!("Passwords don't match.");
        std::process::exit(1);
    }
    if pass1.len() < 8 {
        eprintln!("Password must be at least 8 characters.");
        std::process::exit(1);
    }
    pass1
}
