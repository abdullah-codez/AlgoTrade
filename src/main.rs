//! AlgoTrade Main Application Entry Point
//! Starts the embedded Axum web server on 127.0.0.1:3000 and initializes the trading engine.
//! Invariant: Engine boots strictly STOPPED and never executes trades automatically on boot.

use algo_trade::audit::AuditLogger;
use algo_trade::config::{EngineConfig, MarketMetadata};
use algo_trade::engine::persistence::StatePersistenceManager;
use algo_trade::engine::EngineOrchestrator;
use algo_trade::execution::client::{ExchangeClient, HyperliquidClient};
use algo_trade::execution::mock::MockExchangeClient;
use algo_trade::market_data::feed::HyperliquidWsFeed;
use algo_trade::market_data::MarketFeedEvent;
use algo_trade::server::{start_web_server, DEFAULT_SERVER_ADDR};
use algo_trade::types::{Price, SignerStatus};
use rust_decimal_macros::dec;
use std::sync::Arc;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("=====================================================");
    info!("   AlgoTrade: BTC Perpetual Trading Engine (M3)      ");
    info!("   Target: Hyperliquid Testnet (BTC-PERP)            ");
    info!("=====================================================");

    // 2. Load .env safely without printing secrets
    dotenvy::dotenv().ok();

    // 3. Resolve Credential Configuration
    let account_address = std::env::var("HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS")
        .unwrap_or_else(|_| "0x0000000000000000000000000000000000000001".to_string());
    let agent_private_key = std::env::var("HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY").ok();

    let coin = "BTC";
    let base_rest_url = "https://api.hyperliquid-testnet.xyz";
    let base_ws_url = "wss://api.hyperliquid-testnet.xyz/ws";

    // 4. Instantiate Exchange Client
    let account_configured = !account_address.trim().is_empty()
        && account_address != "0x0000000000000000000000000000000000000000"
        && account_address != "0x0000000000000000000000000000000000000001"
        && !account_address.contains("YOUR_");

    let (exchange_client, is_live_client): (Arc<dyn ExchangeClient>, bool) = if account_configured {
        let key_opt = agent_private_key
            .as_ref()
            .map(|k| k.trim())
            .filter(|k| !k.is_empty() && !k.contains("YOUR_"));

        match key_opt {
            Some(key) => match HyperliquidClient::new(base_rest_url, &account_address, key) {
                Ok(client) => {
                    info!(
                        "Initialized live Hyperliquid Testnet client for target account: {}",
                        account_address
                    );
                    if let Some(agent_addr) = client.agent_address_hex() {
                        info!("Derived Agent Signer Address: {}", agent_addr);
                    }
                    (Arc::new(client), true)
                }
                Err(e) => {
                    warn!(
                        "Agent private key format invalid ({}); using live read-only Hyperliquid client with InvalidKeyFormat",
                        e
                    );
                    let client = HyperliquidClient::new_read_only_with_status(
                        base_rest_url,
                        Some(account_address.clone()),
                        SignerStatus::InvalidKeyFormat,
                    )
                    .expect("Valid URL and address");
                    (Arc::new(client), true)
                }
            },
            None => {
                info!("No agent private key configured; using live read-only Hyperliquid client with MissingKey");
                let client = HyperliquidClient::new_read_only_with_status(
                    base_rest_url,
                    Some(account_address.clone()),
                    SignerStatus::MissingKey,
                )
                .expect("Valid URL and address");
                (Arc::new(client), true)
            }
        }
    } else {
        info!("No valid target account address configured; running in safe local mock mode with MockExchangeClient.");
        let mock = MockExchangeClient::new("BTC", algo_trade::types::SignedUsd::new(dec!(100.0)));
        mock.set_signer_status(SignerStatus::Unconfigured).await;
        (Arc::new(mock), false)
    };

    // 5. Initialize Engine Components
    let audit_logger = Arc::new(AuditLogger::new("events.jsonl"));
    let persistence_manager = Arc::new(StatePersistenceManager::new("session_state.json"));
    let default_config = EngineConfig::normal_default();
    let market_metadata = MarketMetadata::btc_fixture();

    let orchestrator = Arc::new(EngineOrchestrator::new(
        coin,
        &account_address,
        default_config,
        market_metadata,
        exchange_client.clone(),
        audit_logger.clone(),
        persistence_manager.clone(),
    ));

    // Synchronize initial signer status from exchange or client
    orchestrator.sync_signer_status().await;

    // 6. Invariant: Load persisted state, but engine remains strictly STOPPED
    if let Err(e) = orchestrator.load_persisted_state().await {
        warn!("Notice loading persisted state: {}", e);
    }
    info!(
        "Engine initialized in STOPPED state. Trading requires explicit user action via dashboard."
    );

    // Initial background candle recovery across timeframes
    let orch_candle_init = orchestrator.clone();
    tokio::spawn(async move {
        if let Err(e) = orch_candle_init.recover_candle_gaps().await {
            warn!("Startup candle recovery notice: {}", e);
        }
    });

    // 7. Spawn Market Data Feed
    if is_live_client {
        if let Ok(feed) = HyperliquidWsFeed::new(base_ws_url, coin, Some(account_address.clone())) {
            let (mut rx, _handle) = feed.spawn_feed_listener(100);
            let orch_clone = orchestrator.clone();
            tokio::spawn(async move {
                while let Some(event) = rx.recv().await {
                    match event {
                        MarketFeedEvent::MarkPriceUpdate {
                            mark_price,
                            timestamp_ms,
                            ..
                        } => {
                            orch_clone
                                .on_mark_price_tick(mark_price, timestamp_ms)
                                .await;
                        }
                        MarketFeedEvent::FundingUpdate(record) => {
                            orch_clone.on_funding_record(record).await;
                        }
                        MarketFeedEvent::CandleUpdate(candle) => {
                            orch_clone.on_candle_update(candle).await;
                        }
                        _ => {}
                    }
                }
            });
            info!("Live market data feed listener spawned.");
        }
    } else {
        // Mock market data generator: tick around $70,000 every 1 second
        let orch_clone = orchestrator.clone();
        tokio::spawn(async move {
            let mut price = dec!(70000.0);
            let mut up = true;
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
                if up {
                    price += dec!(10.0);
                    if price > dec!(70100.0) {
                        up = false;
                    }
                } else {
                    price -= dec!(10.0);
                    if price < dec!(69900.0) {
                        up = true;
                    }
                }
                let mark_px = Price::new(price).unwrap();
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                orch_clone.on_mark_price_tick(mark_px, now_ms).await;
            }
        });
        info!("Mock market tick generator active (ticking ~$70,000 BTC).");
    }

    // 8. Start Web Server on strictly 127.0.0.1:3000
    info!(
        "Binding Local Web Dashboard strictly to loopback: http://{}",
        DEFAULT_SERVER_ADDR
    );
    if let Err(e) = start_web_server(orchestrator, DEFAULT_SERVER_ADDR, "web").await {
        error!("Web server failed: {}", e);
        return Err(e.into());
    }

    Ok(())
}
