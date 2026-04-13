mod consensus;
mod core;
mod crypto;
mod network;
mod rpc;
mod storage;
mod wallet;

use std::sync::Arc;

use clap::{Parser, Subcommand};
use tokio::sync::Mutex;
use tracing::info;

use crate::core::chain::{AccountState, Blockchain, DEFAULT_MIN_STAKE, GenesisConfig};
use crate::rpc::{RpcRequest, RpcResponse};

const DEFAULT_DATA_DIR: &str = "curs3d_data";
const DEFAULT_RPC_ADDR: &str = "127.0.0.1:9545";

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
        #[arg(long = "bootnode")]
        bootnodes: Vec<String>,
        #[arg(long, default_value = DEFAULT_RPC_ADDR)]
        rpc_addr: String,
        #[arg(long)]
        genesis_config: Option<String>,
    },
    Wallet {
        #[arg(short, long, default_value = "wallet.json")]
        output: String,
    },
    Info {
        #[arg(short, long, default_value = "wallet.json")]
        wallet: String,
    },
    Send {
        #[arg(short, long, default_value = "wallet.json")]
        wallet: String,
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
        #[arg(short, long)]
        amount: u64,
        #[arg(short, long, default_value_t = 1000)]
        fee: u64,
        #[arg(long, default_value = DEFAULT_DATA_DIR)]
        data_dir: String,
        #[arg(long, default_value = DEFAULT_RPC_ADDR)]
        rpc_addr: String,
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
            bootnodes,
            rpc_addr,
            genesis_config,
        } => {
            run_node(
                port,
                &data_dir,
                validator_wallet.as_deref(),
                &bootnodes,
                &rpc_addr,
                genesis_config.as_deref(),
            )
            .await
        }
        Commands::Wallet { output } => create_wallet(&output),
        Commands::Info { wallet: path } => show_wallet_info(&path),
        Commands::Send {
            wallet: path,
            to,
            amount,
            fee,
            data_dir,
            rpc_addr,
        } => send_tokens(&path, &to, amount, fee, &data_dir, &rpc_addr).await,
        Commands::Status { data_dir, rpc_addr } => {
            show_status(&data_dir, rpc_addr.as_deref()).await
        }
        Commands::Stake {
            wallet: path,
            amount,
            fee,
            data_dir,
            rpc_addr,
        } => stake_tokens(&path, amount, fee, &data_dir, &rpc_addr).await,
    }
}

fn create_wallet(path: &str) {
    if wallet::Wallet::exists(path) {
        println!("Wallet already exists at {}", path);
        return;
    }

    let password = prompt_password_create();

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

fn show_wallet_info(path: &str) {
    let password = prompt_password("Enter wallet password: ");
    match wallet::Wallet::load_auto(path, &password) {
        Ok(w) => {
            println!("=== CURS3D Wallet ===");
            println!("Address:    {}", w.address);
            println!("Public Key: {}...", &w.keypair.public_key_hex()[..32]);
            println!("Algorithm:  CRYSTALS-Dilithium (Level 5)");
            println!("Encryption: AES-256-GCM + Argon2");
        }
        Err(wallet::WalletError::WrongPassword) => eprintln!("Error: Wrong password."),
        Err(e) => eprintln!("Failed to load wallet: {}", e),
    }
}

async fn run_node(
    port: u16,
    data_dir: &str,
    validator_wallet: Option<&str>,
    bootnodes: &[String],
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
            eprintln!("Falling back to in-memory mode...");
            match genesis_config {
                Some(config) => match Blockchain::from_genesis(config) {
                    Ok(chain) => chain,
                    Err(err) => {
                        eprintln!("Failed to build in-memory chain from genesis: {}", err);
                        return;
                    }
                },
                None => Blockchain::new(),
            }
        }
    };

    let chain_height = chain.height();
    let latest_hash = chain.latest_block().hash_hex();
    info!(
        "Blockchain loaded. Chain: {}, Height: {}, Latest: {}",
        chain.genesis_config.chain_name,
        chain_height,
        &latest_hash[..16]
    );

    let chain = Arc::new(Mutex::new(chain));

    let validator_key = if let Some(wallet_path) = validator_wallet {
        let password = prompt_password("Enter validator wallet password: ");
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

    let (_outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(100);
    let rpc_chain = Arc::clone(&chain);
    let rpc_addr_owned = rpc_addr.to_string();
    let rpc_task = tokio::spawn(async move { rpc::serve(&rpc_addr_owned, rpc_chain).await });

    match network::NetworkNode::new(port, bootnodes).await {
        Ok(mut node) => {
            let (active_validators, pending_txs, chain_name, genesis_hash) = {
                let chain_lock = chain.lock().await;
                (
                    chain_lock.active_validator_count(),
                    chain_lock.pending_transactions.len(),
                    chain_lock.genesis_config.chain_name.clone(),
                    hex::encode(chain_lock.genesis_hash()),
                )
            };

            println!();
            println!("Chain: {}", chain_name);
            println!("Genesis: {}", genesis_hash);
            println!("Node PeerId: {}", node.peer_id);
            println!("Listening on port {}", port);
            println!("RPC listening on {}", rpc_addr);
            println!("Chain height: {}", chain_height);
            println!("Active validators: {}", active_validators);
            println!("Pending txs: {}", pending_txs);
            println!("Bootnodes: {}", bootnodes.len());
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
                _ = node.run_with_chain(chain, outbound_rx, validator_key) => {}
                rpc_result = rpc_task => {
                    match rpc_result {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => eprintln!("RPC server stopped: {}", err),
                        Err(err) => eprintln!("RPC task failed: {}", err),
                    }
                }
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
    to: &str,
    amount: u64,
    fee: u64,
    data_dir: &str,
    rpc_addr: &str,
) {
    let password = prompt_password("Enter wallet password: ");
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
                println!("Chain:             {}", status.chain_name);
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
    println!("Chain:             {}", chain.genesis_config.chain_name);
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

async fn stake_tokens(wallet_path: &str, amount: u64, fee: u64, data_dir: &str, rpc_addr: &str) {
    let password = prompt_password("Enter wallet password: ");
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

fn prompt_password(prompt: &str) -> String {
    rpassword::prompt_password(prompt).unwrap_or_else(|_| {
        eprintln!("Failed to read password");
        std::process::exit(1);
    })
}

fn prompt_password_create() -> String {
    let pass1 = prompt_password("Create wallet password: ");
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
