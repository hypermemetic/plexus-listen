use clap::Parser;
use plexus_listen::MicHub;
use plexus_core::plexus::DynamicHub;
use plexus_transport::TransportServer;
use std::sync::Arc;

/// CLI arguments for plexus-listen standalone server
#[derive(Parser, Debug)]
#[command(name = "plexus-listen")]
#[command(about = "Audio capture standalone Plexus RPC server — recording, metering, and live streaming")]
struct Args {
    /// Run in stdio mode for MCP compatibility (line-delimited JSON-RPC over stdin/stdout)
    #[arg(long)]
    stdio: bool,

    /// Port for WebSocket server (ignored in stdio mode)
    #[arg(short, long, default_value = "4447")]
    port: u16,

    /// Enable MCP HTTP server (on port + 1)
    #[arg(long)]
    mcp: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Initialize tracing
    let filter = if args.stdio {
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new("plexus_listen=warn,jsonrpsee=warn")
        })
    } else {
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            #[cfg(debug_assertions)]
            let default_filter = "warn,plexus_listen=trace";
            #[cfg(not(debug_assertions))]
            let default_filter = "warn,plexus_listen=debug";
            tracing_subscriber::EnvFilter::new(default_filter)
        })
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("Starting plexus-listen at {}", chrono::Utc::now());

    // Create DynamicHub with audio capture plugin
    let hub = Arc::new(
        DynamicHub::new("listen")
            .register(MicHub::new())
    );

    tracing::info!("plexus-listen initialized");
    tracing::info!("  Namespace: listen");
    tracing::info!("  Activation: mic");
    tracing::info!("  Version: {}", env!("CARGO_PKG_VERSION"));

    // Configure transport server
    let rpc_converter = |arc: Arc<DynamicHub>| {
        DynamicHub::arc_into_rpc_module(arc)
            .map_err(|e| anyhow::anyhow!("Failed to create RPC module: {}", e))
    };

    let mut builder = TransportServer::builder(hub, rpc_converter);

    if args.stdio {
        builder = builder.with_stdio();
    } else {
        builder = builder.with_websocket(args.port);

        if args.mcp {
            builder = builder.with_mcp_http(args.port + 1);
        }
    }

    if args.stdio {
        tracing::info!("Starting stdio transport (MCP-compatible)");
    } else {
        tracing::info!("plexus-listen server started");
        tracing::info!("  WebSocket: ws://127.0.0.1:{}", args.port);
        if args.mcp {
            tracing::info!("  MCP HTTP:  http://127.0.0.1:{}/mcp", args.port + 1);
        }
        tracing::info!("");
        tracing::info!("Usage:");
        tracing::info!("  synapse -P {} listen mic info", args.port);
        tracing::info!("  synapse -P {} listen mic list_devices", args.port);
        tracing::info!("  synapse -P {} listen mic record --path /tmp/out.wav --timeout 5000", args.port);
        tracing::info!("  synapse -P {} listen mic levels", args.port);
        tracing::info!("  synapse -P {} listen mic stream_audio", args.port);
    }

    builder.build().await?.serve().await
}
