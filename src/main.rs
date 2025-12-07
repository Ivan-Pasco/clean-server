//! Clean Server CLI
//!
//! Command-line interface for running compiled Clean Language applications.
//!
//! # Usage
//!
//! ```bash
//! # Run a WASM application
//! clean-server run app.wasm
//!
//! # Run with custom port
//! clean-server run app.wasm --port 8080
//!
//! # Run with verbose logging
//! clean-server run app.wasm --verbose
//!
//! # Show version
//! clean-server --version
//! ```

use clap::Parser;
use clean_server::{start_server, ServerConfig};
use std::path::PathBuf;
use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;

/// Clean Server - HTTP Server for Clean Language Applications
#[derive(Parser, Debug)]
#[command(name = "clean-server")]
#[command(author = "Ivan Pasco <ivan@cleanframework.com>")]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = "Run compiled Clean Language WASM applications", long_about = None)]
struct Args {
    /// Path to the WASM file to execute
    #[arg(value_name = "WASM_FILE")]
    wasm_path: PathBuf,

    /// Port to listen on
    #[arg(short, long, default_value = "3000")]
    port: u16,

    /// Host address to bind to
    #[arg(long, default_value = "0.0.0.0")]
    host: String,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Disable CORS
    #[arg(long)]
    no_cors: bool,

    /// Request body size limit in MB
    #[arg(long, default_value = "10")]
    body_limit: usize,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    // Initialize logging
    let log_level = if args.verbose {
        Level::DEBUG
    } else {
        Level::INFO
    };

    let _subscriber = FmtSubscriber::builder()
        .with_max_level(log_level)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .compact()
        .init();

    // Print banner
    println!();
    println!("  ╔═══════════════════════════════════════════╗");
    println!("  ║         Clean Server v{}            ║", env!("CARGO_PKG_VERSION"));
    println!("  ║   HTTP Server for Clean Language Apps     ║");
    println!("  ╚═══════════════════════════════════════════╝");
    println!();

    // Validate WASM file exists
    if !args.wasm_path.exists() {
        error!("WASM file not found: {:?}", args.wasm_path);
        std::process::exit(1);
    }

    if !args.wasm_path.extension().map_or(false, |ext| ext == "wasm") {
        error!("File must have .wasm extension: {:?}", args.wasm_path);
        std::process::exit(1);
    }

    // Create server config
    let config = ServerConfig {
        host: args.host,
        port: args.port,
        cors_enabled: !args.no_cors,
        cors_origins: vec![],
        body_limit: args.body_limit * 1024 * 1024,
    };

    info!("Configuration:");
    info!("  WASM file: {:?}", args.wasm_path);
    info!("  Listen: {}:{}", config.host, config.port);
    info!("  CORS: {}", if config.cors_enabled { "enabled" } else { "disabled" });
    info!("  Body limit: {} MB", args.body_limit);
    println!();

    // Start server
    match start_server(args.wasm_path, config).await {
        Ok(()) => {
            info!("Server stopped");
        }
        Err(e) => {
            error!("Server error: {}", e);
            std::process::exit(1);
        }
    }
}
