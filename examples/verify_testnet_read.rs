//! Safe Read-Only Testnet Diagnostic Tool
//! Verifies connectivity, metadata, account state, and mark price feed from Hyperliquid Testnet.
//! Guaranteed read-only: Places zero orders, cancels zero orders, performs zero state mutations.
//!
//! Credential Roles:
//! - HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS: Queries account balance, positions, open orders.
//! - HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY: Signs authenticated trading actions (orders, cancels).

use algo_trade::config::{
    validate_account_address, validate_agent_private_key, HyperliquidTestnetConfig,
    ENV_HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS, ENV_HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY,
};
use algo_trade::execution::client::{ExchangeClient, HyperliquidClient};
use algo_trade::execution::signing::HyperliquidSigner;
use algo_trade::market_data::{HyperliquidWsFeed, MarketFeedEvent};
use std::time::Duration;
use tokio::time::timeout;

#[tokio::main]
async fn main() {
    println!("============================================================");
    println!("   BTC PERPETUAL TRADING BOT — TESTNET READ-ONLY DIAGNOSTIC  ");
    println!("============================================================");

    // 1. Load environment from .env if present
    let _ = dotenvy::dotenv();

    let config = match HyperliquidTestnetConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("  [FAIL] Configuration error: {}", e);
            return;
        }
    };

    println!("\n[1/5] Enforcing Testnet Network Guard Allowlist...");
    println!("  [OK] REST URL validated: {}", config.api_url);
    println!("  [OK] WS URL validated:   {}", config.ws_url);

    // 2. Validate Credential Roles
    println!("\n[2/5] Inspecting & Validating Credential Roles...");

    let account_addr_opt = config.account_address.clone();
    let agent_key_opt = config.agent_private_key.clone();

    match &account_addr_opt {
        Some(addr) => match validate_account_address(addr) {
            Ok(()) => println!("  [OK] Account Address (Trading Account): {}", addr),
            Err(e) => println!("  [WARN] Account Address validation failed: {}", e),
        },
        None => println!(
            "  [INFO] {} is not set (required for account queries)",
            ENV_HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS
        ),
    }

    let _agent_signer = match &agent_key_opt {
        Some(key) => match validate_agent_private_key(key) {
            Ok(()) => match HyperliquidSigner::new(key) {
                Ok(signer) => {
                    let agent_addr = signer.wallet_address_hex();
                    println!("  [OK] Agent Wallet Address (Signer):     {}", agent_addr);
                    if let Some(ref acct) = account_addr_opt {
                        if acct.eq_ignore_ascii_case(&agent_addr) {
                            println!("       Notice: Account address matches agent address.");
                        } else {
                            println!("       Role separation verified: Agent signs on behalf of Account.");
                        }
                    }
                    Some(signer)
                }
                Err(e) => {
                    println!("  [WARN] Failed to parse agent private key: {}", e);
                    None
                }
            },
            Err(e) => {
                println!("  [WARN] Agent private key validation failed: {}", e);
                None
            }
        },
        None => {
            println!(
                "  [INFO] {} is not set (read-only mode active)",
                ENV_HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY
            );
            None
        }
    };

    // 3. Initialize REST Client & Query Market Metadata
    println!("\n[3/5] Querying BTC Market Metadata from /info...");
    let dummy_test_account = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";
    let target_account = account_addr_opt.as_deref().unwrap_or(dummy_test_account);

    let client =
        match HyperliquidClient::new_read_only(&config.api_url, Some(target_account.to_string())) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("  [FAIL] Failed to initialize client: {}", e);
                return;
            }
        };

    match client.get_metadata("BTC").await {
        Ok(meta) => {
            println!("  [OK] Market Metadata Retrieved:");
            println!("       Coin:         {}", meta.coin);
            println!(
                "       szDecimals:   {} (min increment: {})",
                meta.sz_decimals,
                meta.min_size_increment()
            );
            println!("       maxLeverage:  {}x", meta.max_leverage);
        }
        Err(e) => {
            eprintln!(
                "  [WARN] Failed to query metadata (network offline or testnet unreachable): {}",
                e
            );
        }
    }

    // 4. Query Account State and Open Orders (Using Target Trading Account)
    println!(
        "\n[4/5] Querying Clearinghouse Account State (Target: {})...",
        target_account
    );
    match client.get_user_state(target_account).await {
        Ok(state) => {
            println!("  [OK] Clearinghouse State:");
            println!("       Account Value:       {}", state.account_value);
            println!("       Total Margin Used:   {}", state.total_margin_used);
            println!("       Withdrawable Margin: {}", state.withdrawable);
            println!("       Open Positions:      {}", state.positions.len());
            for pos in &state.positions {
                println!(
                    "         - {} {}: szi={}, entry_px={:?}, unrealized_pnl={}",
                    if pos.is_long { "LONG" } else { "SHORT" },
                    pos.coin,
                    pos.szi,
                    pos.entry_px,
                    pos.unrealized_pnl
                );
            }
        }
        Err(e) => {
            eprintln!("  [WARN] Failed to query user state: {}", e);
        }
    }

    match client.get_open_orders(target_account).await {
        Ok(orders) => {
            println!("       Active Resting Orders: {}", orders.len());
            for o in &orders {
                println!(
                    "         - oid: {}, side: {:?}, sz: {}, limit_px: {}, is_trigger: {}",
                    o.oid, o.side, o.size, o.limit_px, o.is_trigger
                );
            }
        }
        Err(e) => {
            eprintln!("  [WARN] Failed to query open orders: {}", e);
        }
    }

    // 5. Test WebSocket Mark Price Stream
    println!("\n[5/5] Testing Real-Time WebSocket Mark Price Feed (timeout: 5s)...");
    match HyperliquidWsFeed::new(&config.ws_url, "BTC", Some(target_account.to_string())) {
        Ok(feed) => {
            let (mut rx, handle) = feed.spawn_feed_listener(16);
            let ws_test = async {
                while let Some(event) = rx.recv().await {
                    match event {
                        MarketFeedEvent::Connected => {
                            println!("  [OK] WebSocket Connected");
                        }
                        MarketFeedEvent::MarkPriceUpdate {
                            coin,
                            mark_price,
                            timestamp_ms,
                        } => {
                            println!(
                                "  [OK] Mark Price Tick Received: {} = ${} (timestamp: {})",
                                coin, mark_price, timestamp_ms
                            );
                            return true;
                        }
                        MarketFeedEvent::Disconnected(reason) => {
                            println!("  [INFO] WebSocket event: disconnected ({})", reason);
                        }
                        _ => {}
                    }
                }
                false
            };

            match timeout(Duration::from_secs(5), ws_test).await {
                Ok(true) => println!("  WebSocket verification successful!"),
                Ok(false) => println!("  WebSocket stream ended without mark price."),
                Err(_) => println!("  [TIMEOUT] No mark price tick received within 5s (offline or no active testnet ticks)."),
            }

            handle.abort();
        }
        Err(e) => {
            eprintln!("  [FAIL] Failed to construct WebSocket feed: {}", e);
        }
    }

    println!("\n============================================================");
    println!("   DIAGNOSTIC COMPLETE — ZERO ORDERS PLACED OR CANCELLED    ");
    println!("============================================================");
}
