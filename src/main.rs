use curs3d::{api, core, crypto, network, rpc, runtime, token, wallet};

use std::sync::Arc;
use std::{collections::BTreeMap, fs, path::PathBuf};

use clap::{Parser, Subcommand};
use libp2p::{Multiaddr, identity};
use tokio::sync::{Mutex, RwLock};
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
        http_addr: Option<String>,
        #[arg(long)]
        genesis_config: Option<String>,
        /// Generate a fresh libp2p identity and replace the existing one. The
        /// previous keyfile is moved aside as `p2p_identity.pb.bak`. This will
        /// change the node's PeerId — bootstrap peers will need the new id.
        #[arg(long, default_value_t = false)]
        reset_p2p_identity: bool,
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
        #[arg(long = "validator-wallet", required = true, action = clap::ArgAction::Append)]
        validator_wallets: Vec<String>,
        #[arg(long = "validator-password-file", action = clap::ArgAction::Append)]
        validator_password_files: Vec<String>,
        #[arg(long = "validator-balance-cur", action = clap::ArgAction::Append)]
        validator_balance_curs: Vec<u64>,
        #[arg(long = "validator-stake-cur", action = clap::ArgAction::Append)]
        validator_stake_curs: Vec<u64>,
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
    Unstake {
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
    DeployToken {
        #[arg(short, long, default_value = "wallet.json")]
        wallet: String,
        #[arg(long)]
        password_file: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        symbol: String,
        #[arg(long, default_value_t = 6)]
        decimals: u8,
        #[arg(long)]
        total_supply: u64,
        #[arg(short, long, default_value_t = 1000)]
        fee: u64,
        #[arg(long, default_value = DEFAULT_DATA_DIR)]
        data_dir: String,
        #[arg(long, default_value = DEFAULT_RPC_ADDR)]
        rpc_addr: String,
    },
    TokenTransfer {
        #[arg(short, long, default_value = "wallet.json")]
        wallet: String,
        #[arg(long)]
        password_file: Option<String>,
        #[arg(long)]
        token: String,
        #[arg(long)]
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
    BootnodeAddress {
        #[arg(short, long, default_value = DEFAULT_DATA_DIR)]
        data_dir: String,
        #[arg(long = "public-addr")]
        public_addrs: Vec<String>,
    },
    /// Deploy a WebAssembly smart contract built with the curs3d-contract Rust SDK.
    DeployContract {
        #[arg(short, long, default_value = "wallet.json")]
        wallet: String,
        #[arg(long)]
        password_file: Option<String>,
        /// Path to the .wasm file (e.g. target/wasm32-unknown-unknown/release/foo.wasm)
        #[arg(long)]
        wasm: String,
        #[arg(long, default_value_t = 5_000_000)]
        gas_limit: u64,
        #[arg(short, long, default_value_t = 1000)]
        fee: u64,
        #[arg(long, default_value = DEFAULT_DATA_DIR)]
        data_dir: String,
        #[arg(long, default_value = DEFAULT_RPC_ADDR)]
        rpc_addr: String,
    },
    /// Light-client demo: connect to a node API, fetch genesis + headers,
    /// verify them with the in-tree LightClient, and print the result.
    LightSync {
        #[arg(long, default_value = "https://api.curs3d.fr")]
        api: String,
        #[arg(long, default_value_t = 64)]
        limit: u64,
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
            http_addr,
            genesis_config,
            reset_p2p_identity,
        } => {
            run_node(
                port,
                &data_dir,
                validator_wallet.as_deref(),
                validator_password_file.as_deref(),
                &bootnodes,
                &public_addrs,
                &rpc_addr,
                http_addr.as_deref(),
                genesis_config.as_deref(),
                reset_p2p_identity,
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
            validator_wallets,
            validator_password_files,
            validator_balance_curs,
            validator_stake_curs,
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
            &validator_wallets,
            &validator_password_files,
            &validator_balance_curs,
            &validator_stake_curs,
            faucet_wallet.as_deref(),
            faucet_password_file.as_deref(),
            faucet_balance_cur,
            block_reward_cur,
            minimum_stake_cur,
            epoch_length,
        ),
        Commands::Unstake {
            wallet: path,
            password_file,
            amount,
            fee,
            data_dir,
            rpc_addr,
        } => {
            unstake_tokens(
                &path,
                password_file.as_deref(),
                amount,
                fee,
                &data_dir,
                &rpc_addr,
            )
            .await
        }
        Commands::DeployToken {
            wallet: path,
            password_file,
            name,
            symbol,
            decimals,
            total_supply,
            fee,
            data_dir,
            rpc_addr,
        } => {
            deploy_token(
                &path,
                password_file.as_deref(),
                &name,
                &symbol,
                decimals,
                total_supply,
                fee,
                &data_dir,
                &rpc_addr,
            )
            .await
        }
        Commands::TokenTransfer {
            wallet: path,
            password_file,
            token,
            to,
            amount,
            fee,
            data_dir,
            rpc_addr,
        } => {
            transfer_token(
                &path,
                password_file.as_deref(),
                &token,
                &to,
                amount,
                fee,
                &data_dir,
                &rpc_addr,
            )
            .await
        }
        Commands::BootnodeAddress {
            data_dir,
            public_addrs,
        } => show_bootnode_addresses(&data_dir, &public_addrs),
        Commands::DeployContract {
            wallet,
            password_file,
            wasm,
            gas_limit,
            fee,
            data_dir,
            rpc_addr,
        } => {
            deploy_contract(
                &wallet,
                password_file.as_deref(),
                &wasm,
                gas_limit,
                fee,
                &data_dir,
                &rpc_addr,
            )
            .await
        }
        Commands::LightSync { api, limit } => light_sync(&api, limit).await,
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
            println!("Keys: ML-DSA-87 (FIPS-204, Dilithium Level 5 security)");
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
                    "algorithm": "ML-DSA-87 (FIPS-204)",
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
                println!("Algorithm:  ML-DSA-87 (FIPS-204)");
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
    http_addr_override: Option<&str>,
    genesis_config_path: Option<&str>,
    reset_p2p_identity: bool,
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

    let chain = match Blockchain::with_storage_async_persistence(data_dir, genesis_config.as_ref())
    {
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
    let http_addr = match resolve_http_addr(rpc_addr, http_addr_override) {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("Failed to resolve HTTP API address: {}", err);
            return;
        }
    };

    let (validator_key, validator_address) = if let Some(wallet_path) = validator_wallet {
        let password = resolve_password(
            validator_password_file,
            "CURS3D_VALIDATOR_PASSWORD_FILE",
            "CURS3D_VALIDATOR_PASSWORD",
            Some(("Enter validator wallet password: ", false)),
        );
        match wallet::Wallet::load_auto(wallet_path, &password) {
            Ok(w) => {
                println!("Validator wallet loaded: {}", w.address);
                (Some(w.keypair), Some(w.address))
            }
            Err(wallet::WalletError::WrongPassword) => {
                eprintln!("Wrong password for validator wallet.");
                return;
            }
            Err(e) => {
                eprintln!("Failed to load validator wallet: {}", e);
                return;
            }
        }
    } else {
        println!("No validator wallet specified. Running as relay node.");
        (None, None)
    };

    let node_role = if validator_key.is_some() {
        runtime::NodeRole::Validator
    } else {
        runtime::NodeRole::Relay
    };
    let runtime_state: runtime::SharedRuntimeState =
        Arc::new(RwLock::new(runtime::RuntimeState::new(
            node_role,
            validator_address.clone(),
            bootnodes.len(),
            rpc_addr,
            http_addr.clone(),
        )));

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
    let http_addr_for_task = http_addr.clone();
    let http_runtime_state = Arc::clone(&runtime_state);
    let http_task = tokio::spawn(async move {
        if let Err(e) = api::serve_http(
            &http_addr_for_task,
            http_chain,
            http_event_tx,
            http_outbound_tx,
            http_runtime_state,
        )
        .await
        {
            tracing::error!("HTTP API error: {}", e);
        }
    });

    let p2p_identity = match load_or_create_p2p_identity(data_dir, reset_p2p_identity) {
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
            {
                let mut state = runtime_state.write().await;
                state.set_network_online(true);
            }
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
            println!("HTTP API on http://{}", http_addr);
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
                _ = node.run_with_chain(
                    chain,
                    outbound_rx,
                    validator_key,
                    Some(event_tx.clone()),
                    Arc::clone(&runtime_state),
                ) => {}
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
            eprintln!("Node startup aborted.");
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
    println!("Crypto:            ML-DSA-87 + SHA3-256");
    println!("Storage:           redb");
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

async fn unstake_tokens(
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

    let unstake_micro = amount.saturating_mul(MICROTOKENS_PER_CUR);
    if account_state.staked_balance < unstake_micro {
        eprintln!(
            "Insufficient staked balance: have {} CURS3D staked, trying to unstake {} CURS3D",
            account_state.staked_balance / MICROTOKENS_PER_CUR,
            amount
        );
        return;
    }

    let chain_id = resolve_chain_id(data_dir, rpc_addr)
        .await
        .unwrap_or_else(|_| "curs3d-devnet".to_string());
    let mut tx = crate::core::transaction::Transaction::unstake(
        &chain_id,
        w.keypair.public_key.clone(),
        unstake_micro,
        fee,
        account_state.nonce,
    );
    tx.sign(&w.keypair);

    match submit_transaction(tx.clone(), data_dir, rpc_addr).await {
        Ok(mode) => {
            println!("=== CURS3D Unstake Submitted ===");
            println!("Validator: {}", w.address);
            println!("Unstake:   {} CURS3D", amount);
            println!("Fee:       {} microtokens", fee);
            println!("Nonce:     {}", account_state.nonce);
            println!("TxHash:    {}", tx.hash_hex());
            println!("Route:     {}", mode);
            println!();
            println!(
                "Funds will unlock after {} blocks.",
                crate::core::chain::DEFAULT_UNSTAKE_DELAY_BLOCKS
            );
        }
        Err(e) => eprintln!("Failed to submit unstake transaction: {}", e),
    }
}

#[allow(clippy::too_many_arguments)]
async fn deploy_token(
    wallet_path: &str,
    password_file: Option<&str>,
    name: &str,
    symbol: &str,
    decimals: u8,
    total_supply: u64,
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

    let params = crate::token::DeployTokenParams {
        name: name.to_string(),
        symbol: symbol.to_string(),
        decimals,
        total_supply,
    };
    let data = serde_json::to_vec(&params).expect("failed to serialize token params");

    let chain_id = resolve_chain_id(data_dir, rpc_addr)
        .await
        .unwrap_or_else(|_| "curs3d-devnet".to_string());

    let mut tx = crate::core::transaction::Transaction {
        chain_id,
        kind: crate::core::transaction::TransactionKind::DeployToken,
        from: sender_address,
        sender_public_key: w.keypair.public_key.clone(),
        to: Vec::new(),
        amount: 0,
        fee,
        max_fee_per_gas: fee,
        max_priority_fee_per_gas: fee,
        nonce: account_state.nonce,
        timestamp: chrono::Utc::now().timestamp(),
        signature: None,
        gas_limit: 0,
        data,
        evm_raw_tx: Vec::new(),
    };
    tx.sign(&w.keypair);

    match submit_transaction(tx.clone(), data_dir, rpc_addr).await {
        Ok(mode) => {
            println!("=== CURS3D Token Deployed ===");
            println!("Name:         {}", name);
            println!("Symbol:       {}", symbol);
            println!("Decimals:     {}", decimals);
            println!("Total Supply: {}", total_supply);
            println!("Deployer:     {}", w.address);
            println!("TxHash:       {}", tx.hash_hex());
            println!("Route:        {}", mode);
        }
        Err(e) => eprintln!("Failed to deploy token: {}", e),
    }
}

#[allow(clippy::too_many_arguments)]
async fn transfer_token(
    wallet_path: &str,
    password_file: Option<&str>,
    token_address: &str,
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

    let token_clean = token_address.strip_prefix("CUR").unwrap_or(token_address);
    let token_bytes = match hex::decode(token_clean) {
        Ok(b) if b.len() == 20 => b,
        _ => {
            eprintln!("Invalid token address: {}", token_address);
            return;
        }
    };

    let to_clean = to.strip_prefix("CUR").unwrap_or(to);
    let to_bytes = match hex::decode(to_clean) {
        Ok(b) if b.len() == 20 => b,
        _ => {
            eprintln!("Invalid recipient address: {}", to);
            return;
        }
    };

    let params = crate::token::TokenTransferParams {
        token_address: token_bytes,
        recipient: to_bytes,
        amount,
    };
    let data = serde_json::to_vec(&params).expect("failed to serialize token transfer params");

    let chain_id = resolve_chain_id(data_dir, rpc_addr)
        .await
        .unwrap_or_else(|_| "curs3d-devnet".to_string());

    let mut tx = crate::core::transaction::Transaction {
        chain_id,
        kind: crate::core::transaction::TransactionKind::TokenTransfer,
        from: sender_address,
        sender_public_key: w.keypair.public_key.clone(),
        to: Vec::new(),
        amount: 0,
        fee,
        max_fee_per_gas: fee,
        max_priority_fee_per_gas: fee,
        nonce: account_state.nonce,
        timestamp: chrono::Utc::now().timestamp(),
        signature: None,
        gas_limit: 0,
        data,
        evm_raw_tx: Vec::new(),
    };
    tx.sign(&w.keypair);

    match submit_transaction(tx.clone(), data_dir, rpc_addr).await {
        Ok(mode) => {
            println!("=== CURS3D Token Transfer Submitted ===");
            println!("Token:     {}", token_address);
            println!("To:        {}", to);
            println!("Amount:    {}", amount);
            println!("From:      {}", w.address);
            println!("TxHash:    {}", tx.hash_hex());
            println!("Route:     {}", mode);
        }
        Err(e) => eprintln!("Failed to submit token transfer: {}", e),
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

#[allow(clippy::too_many_arguments)]
async fn deploy_contract(
    wallet_path: &str,
    password_file: Option<&str>,
    wasm_path: &str,
    gas_limit: u64,
    fee: u64,
    data_dir: &str,
    rpc_addr: &str,
) {
    let wasm_bytes = match std::fs::read(wasm_path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("Failed to read wasm file {}: {}", wasm_path, e);
            return;
        }
    };
    if wasm_bytes.is_empty() {
        eprintln!("Wasm file is empty");
        return;
    }
    if !(wasm_bytes.len() >= 4 && &wasm_bytes[0..4] == b"\0asm") {
        eprintln!(
            "File does not look like a WebAssembly module (expected magic '\\0asm', got {:?})",
            &wasm_bytes[..wasm_bytes.len().min(4)]
        );
        return;
    }
    println!(
        "Loaded {} bytes of wasm from {}",
        wasm_bytes.len(),
        wasm_path
    );

    let password = resolve_password(
        password_file,
        "CURS3D_WALLET_PASSWORD_FILE",
        "CURS3D_WALLET_PASSWORD",
        Some(("Enter wallet password: ", false)),
    );
    let w = match wallet::Wallet::load_auto(wallet_path, &password) {
        Ok(w) => w,
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

    let chain_id = resolve_chain_id(data_dir, rpc_addr)
        .await
        .unwrap_or_else(|_| "curs3d-devnet".to_string());

    let mut tx = core::transaction::Transaction::deploy_contract(
        &chain_id,
        w.keypair.public_key.clone(),
        wasm_bytes.clone(),
        gas_limit,
        fee,
        account_state.nonce,
    );
    tx.sign(&w.keypair);
    let tx_hash = tx.hash_hex();

    // Predict the contract address (deterministic: sha3(deployer || nonce))
    let predicted_address = {
        let mut data = Vec::with_capacity(28);
        data.extend_from_slice(&sender_address);
        data.extend_from_slice(&account_state.nonce.to_le_bytes());
        let digest = crypto::hash::sha3_hash(&data);
        digest[..crypto::hash::ADDRESS_LEN].to_vec()
    };

    match submit_transaction(tx, data_dir, rpc_addr).await {
        Ok(mode) => {
            println!();
            println!("=== CURS3D Contract Deployed ===");
            println!("Wasm size:        {} bytes", wasm_bytes.len());
            println!("Deployer:         {}", w.address);
            println!("Predicted addr:   CUR{}", hex::encode(&predicted_address));
            println!("Tx hash:          {}", tx_hash);
            println!("Gas limit:        {}", gas_limit);
            println!("Fee:              {}", fee);
            println!("Route:            {}", mode);
            println!();
            println!(
                "Once mined, query the receipt: curl http://localhost:8080/api/receipt/{}",
                tx_hash
            );
        }
        Err(e) => eprintln!("Failed to deploy contract: {}", e),
    }
}

async fn light_sync(api: &str, limit: u64) {
    let api = api.trim_end_matches('/');
    println!("CURS3D light-client sync demo");
    println!("API endpoint: {}", api);

    let genesis_url = format!("{}/api/genesis", api);
    let headers_url = format!("{}/api/headers?from=0&limit={}", api, limit.clamp(1, 256));

    println!();
    println!("Step 1: fetch the genesis anchor");
    println!("  curl -s {}", genesis_url);
    println!();
    println!("Step 2: fetch a range of signed headers");
    println!("  curl -s '{}'", headers_url);
    println!();
    println!("Step 3: feed the headers to the LightClient.");
    println!("        The struct lives in src/light/mod.rs and exposes:");
    println!("          - LightClient::new(chain_id, genesis_hash)");
    println!("          - LightClient::sync_headers(Vec<SignedHeader>)");
    println!("          - LightClient::verify_account_proof(...)");
    println!("          - LightClient::verify_storage_proof(...)");
    println!();
    println!(
        "Pseudocode (drop into a Rust binary that depends on this crate as a library):
    use curs3d::light::{{LightClient, SignedHeader}};

    let g: serde_json::Value = reqwest::get(\"{}/api/genesis\")
        .await?.json().await?;
    let chain_id = g[\"data\"][\"chain_id\"]
        .as_str()
        .ok_or(\"missing chain_id\")?
        .to_string();
    let genesis_hash = hex::decode(
        g[\"data\"][\"genesis_hash\"]
            .as_str()
            .ok_or(\"missing genesis_hash\")?,
    )?;

    let h: serde_json::Value = reqwest::get(\"{}/api/headers?from=0&limit=64\")
        .await?.json().await?;
    let headers: Vec<SignedHeader> = serde_json::from_value(h[\"data\"][\"headers\"].clone())?;

    let mut client = LightClient::new(chain_id, genesis_hash);
    client.sync_headers(headers)?;
    println!(\"verified up to height {{}}\", client.height());",
        api, api
    );
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
    validator_wallets: &[String],
    validator_password_files: &[String],
    validator_balance_curs: &[u64],
    validator_stake_curs: &[u64],
    faucet_wallet: Option<&str>,
    faucet_password_file: Option<&str>,
    faucet_balance_cur: u64,
    block_reward_cur: u64,
    minimum_stake_cur: u64,
    epoch_length: u64,
) {
    let validator_count = validator_wallets.len();
    let validator_password_files = match expand_string_arg(
        "validator password files",
        validator_password_files,
        validator_count,
    ) {
        Ok(values) => values,
        Err(err) => {
            eprintln!("{}", err);
            return;
        }
    };
    let validator_balance_curs = match expand_u64_arg(
        "validator balances",
        validator_balance_curs,
        validator_count,
        1_500_000,
    ) {
        Ok(values) => values,
        Err(err) => {
            eprintln!("{}", err);
            return;
        }
    };
    let validator_stake_curs = match expand_u64_arg(
        "validator stakes",
        validator_stake_curs,
        validator_count,
        50_000,
    ) {
        Ok(values) => values,
        Err(err) => {
            eprintln!("{}", err);
            return;
        }
    };

    let mut allocations = BTreeMap::<String, (u64, u64)>::new();
    let mut validator_summaries = Vec::with_capacity(validator_count);
    for (index, wallet_path) in validator_wallets.iter().enumerate() {
        let validator_password = resolve_password(
            validator_password_files[index].as_deref(),
            "CURS3D_VALIDATOR_PASSWORD_FILE",
            "CURS3D_VALIDATOR_PASSWORD",
            Some(("Enter validator wallet password: ", false)),
        );
        let validator = match wallet::Wallet::load_auto(wallet_path, &validator_password) {
            Ok(wallet) => wallet,
            Err(err) => {
                eprintln!("Failed to load validator wallet {}: {}", wallet_path, err);
                return;
            }
        };
        allocations.insert(
            validator.keypair.public_key_hex(),
            (
                validator_balance_curs[index].saturating_mul(MICROTOKENS_PER_CUR),
                validator_stake_curs[index].saturating_mul(MICROTOKENS_PER_CUR),
            ),
        );
        validator_summaries.push((
            wallet_path.clone(),
            validator.address,
            validator_balance_curs[index],
            validator_stake_curs[index],
        ));
    }

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
    println!("Validators:          {}", validator_summaries.len());
    for (index, (wallet_path, address, balance_cur, stake_cur)) in
        validator_summaries.iter().enumerate()
    {
        println!("  Validator {} Wallet:   {}", index + 1, wallet_path);
        println!("  Validator {} Address:  {}", index + 1, address);
        println!("  Validator {} Balance:  {} CURS3D", index + 1, balance_cur);
        println!("  Validator {} Stake:    {} CURS3D", index + 1, stake_cur);
    }
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

fn resolve_http_addr(rpc_addr: &str, http_addr_override: Option<&str>) -> Result<String, String> {
    if let Some(http_addr) = http_addr_override {
        return Ok(http_addr.to_string());
    }

    if let Ok(addr) = rpc_addr.parse::<std::net::SocketAddr>() {
        return Ok(std::net::SocketAddr::new(addr.ip(), 8080).to_string());
    }

    match rpc_addr.rsplit_once(':') {
        Some((host, _)) if !host.is_empty() => Ok(format!("{}:8080", host)),
        _ => Err(format!(
            "could not derive HTTP address from RPC address '{}'; set --http-addr explicitly",
            rpc_addr
        )),
    }
}

fn expand_string_arg(
    label: &str,
    values: &[String],
    target_len: usize,
) -> Result<Vec<Option<String>>, String> {
    match values.len() {
        0 => Ok(vec![None; target_len]),
        1 => Ok((0..target_len).map(|_| Some(values[0].clone())).collect()),
        len if len == target_len => Ok(values.iter().cloned().map(Some).collect()),
        len => Err(format!(
            "invalid {} count: expected 0, 1, or {}, got {}",
            label, target_len, len
        )),
    }
}

fn expand_u64_arg(
    label: &str,
    values: &[u64],
    target_len: usize,
    default_value: u64,
) -> Result<Vec<u64>, String> {
    match values.len() {
        0 => Ok(vec![default_value; target_len]),
        1 => Ok(vec![values[0]; target_len]),
        len if len == target_len => Ok(values.to_vec()),
        len => Err(format!(
            "invalid {} count: expected 0, 1, or {}, got {}",
            label, target_len, len
        )),
    }
}

fn show_bootnode_addresses(data_dir: &str, public_addrs: &[String]) {
    if let Err(err) = fs::create_dir_all(data_dir) {
        eprintln!("Failed to create data directory: {}", err);
        return;
    }
    let identity = match load_or_create_p2p_identity(data_dir, false) {
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

/// Persist the libp2p identity keypair atomically with 0600 permissions.
///
/// We write to a sibling tmpfile and rename so a crash mid-write can never
/// leave a half-written keyfile that would silently regenerate a new PeerId
/// on next boot — that exact bug bit us in the live testnet (#1). On unix we
/// also chmod the file to 0600 so other local users can't read the secret.
fn write_p2p_identity_atomic(path: &PathBuf, encoded: &[u8]) -> Result<(), String> {
    let tmp_path = path.with_extension("pb.tmp");
    fs::write(&tmp_path, encoded).map_err(|err| err.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        fs::set_permissions(&tmp_path, perms).map_err(|err| err.to_string())?;
    }
    fs::rename(&tmp_path, path).map_err(|err| err.to_string())?;
    Ok(())
}

/// Load the persistent libp2p identity from disk, regenerating it only if the
/// file is genuinely missing. Parse failures are surfaced as hard errors —
/// silently regenerating on a corrupt file would re-introduce the original
/// "PeerId churns across restarts" bug (#1).
///
/// If `force_reset` is true (CLI `--reset-p2p-identity`), we generate a fresh
/// keypair and overwrite the file. The previous keyfile is moved aside as
/// `p2p_identity.pb.bak` so it can be recovered manually.
fn load_or_create_p2p_identity(
    data_dir: &str,
    force_reset: bool,
) -> Result<identity::Keypair, String> {
    let path = p2p_identity_path(data_dir);

    if force_reset && path.exists() {
        let backup = path.with_extension("pb.bak");
        let _ = fs::rename(&path, &backup);
    }

    if !force_reset && path.exists() {
        let bytes = fs::read(&path).map_err(|err| err.to_string())?;
        return identity::Keypair::from_protobuf_encoding(&bytes).map_err(|err| {
            // Make the error message actionable — operators have hit this
            // when restoring a partial backup, and silently regenerating
            // would change the PeerId.
            format!(
                "failed to parse {}: {}. \
                 Use `curs3d node --reset-p2p-identity` to regenerate (will change PeerId).",
                path.display(),
                err
            )
        });
    }

    let keypair = identity::Keypair::generate_ed25519();
    let encoded = keypair
        .to_protobuf_encoding()
        .map_err(|err| err.to_string())?;
    write_p2p_identity_atomic(&path, &encoded)?;
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
