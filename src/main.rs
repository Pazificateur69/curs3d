mod consensus;
mod core;
mod crypto;
mod network;
mod wallet;

use clap::{Parser, Subcommand};
use tracing::info;

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
    /// Initialize and run a CURS3D node
    Node {
        /// Port to listen on
        #[arg(short, long, default_value_t = 4337)]
        port: u16,
    },
    /// Create a new wallet
    Wallet {
        /// Path to save wallet file
        #[arg(short, long, default_value = "wallet.json")]
        output: String,
    },
    /// Show wallet info
    Info {
        /// Path to wallet file
        #[arg(short, long, default_value = "wallet.json")]
        wallet: String,
    },
    /// Send tokens
    Send {
        /// Wallet file
        #[arg(short, long, default_value = "wallet.json")]
        wallet: String,
        /// Recipient address
        #[arg(short, long)]
        to: String,
        /// Amount to send
        #[arg(short, long)]
        amount: u64,
    },
    /// Show blockchain status
    Status,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();

    match cli.command {
        Commands::Node { port } => {
            run_node(port).await;
        }
        Commands::Wallet { output } => {
            create_wallet(&output);
        }
        Commands::Info { wallet: path } => {
            show_wallet_info(&path);
        }
        Commands::Send {
            wallet: path,
            to,
            amount,
        } => {
            send_tokens(&path, &to, amount).await;
        }
        Commands::Status => {
            show_status();
        }
    }
}

fn create_wallet(path: &str) {
    if wallet::Wallet::exists(path) {
        println!("Wallet already exists at {}", path);
        return;
    }

    let w = wallet::Wallet::new();
    w.save(path).expect("failed to save wallet");

    println!("=== CURS3D Wallet Created ===");
    println!("Address: {}", w.address);
    println!("Saved to: {}", path);
    println!();
    println!("IMPORTANT: Keep your wallet file safe!");
    println!("Your keys are quantum-resistant (CRYSTALS-Dilithium).");
}

fn show_wallet_info(path: &str) {
    match wallet::Wallet::load(path) {
        Ok(w) => {
            println!("=== CURS3D Wallet ===");
            println!("Address:    {}", w.address);
            println!("Public Key: {}...", &w.keypair.public_key_hex()[..32]);
            println!("Algorithm:  CRYSTALS-Dilithium (Level 5)");
        }
        Err(e) => {
            eprintln!("Failed to load wallet: {}", e);
        }
    }
}

async fn run_node(port: u16) {
    println!(r#"
   ██████╗██╗   ██╗██████╗ ███████╗██████╗ ██████╗
  ██╔════╝██║   ██║██╔══██╗██╔════╝╚════██╗██╔══██╗
  ██║     ██║   ██║██████╔╝███████╗ █████╔╝██║  ██║
  ██║     ██║   ██║██╔══██╗╚════██║ ╚═══██╗██║  ██║
  ╚██████╗╚██████╔╝██║  ██║███████║██████╔╝██████╔╝
   ╚═════╝ ╚═════╝ ╚═╝  ╚═╝╚══════╝╚═════╝ ╚═════╝
                 Quantum-Resistant Blockchain
    "#);

    println!("Starting CURS3D node on port {}...", port);

    let chain = core::chain::Blockchain::new();
    info!("Blockchain initialized. Genesis block: {}", chain.latest_block().hash_hex());

    let (_net_tx, net_rx) = tokio::sync::mpsc::channel(100);
    let (msg_tx, mut _msg_rx) = tokio::sync::mpsc::channel(100);

    match network::NetworkNode::new(port).await {
        Ok(mut node) => {
            println!("Node PeerId: {}", node.peer_id);
            println!("Listening on port {}", port);
            println!("Waiting for peers...");
            println!();
            println!("Press Ctrl+C to stop the node.");

            node.run(net_rx, msg_tx).await;
        }
        Err(e) => {
            eprintln!("Failed to start network node: {}", e);
            eprintln!("Running in offline mode...");

            println!("Blockchain running in offline mode.");
            println!("Height: {}", chain.height());
            println!("Genesis: {}", chain.latest_block().hash_hex());

            tokio::signal::ctrl_c().await.expect("failed to listen for ctrl-c");
            println!("\nShutting down...");
        }
    }
}

async fn send_tokens(wallet_path: &str, to: &str, amount: u64) {
    let w = match wallet::Wallet::load(wallet_path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("Failed to load wallet: {}", e);
            return;
        }
    };

    println!("=== CURS3D Transaction ===");
    println!("From:   {}", w.address);
    println!("To:     {}", to);
    println!("Amount: {} CURS3D", amount);
    println!();
    println!("Transaction created (node required to broadcast)");
}

fn show_status() {
    let chain = core::chain::Blockchain::new();
    println!("=== CURS3D Blockchain Status ===");
    println!("Height:       {}", chain.height());
    println!("Genesis Hash: {}", chain.latest_block().hash_hex());
    println!("Block Reward: {} CURS3D", chain.block_reward);
    println!("Consensus:    Proof of Stake");
    println!("Crypto:       CRYSTALS-Dilithium (Post-Quantum)");
    println!("Hash:         SHA-3 (Keccak-256)");
}
