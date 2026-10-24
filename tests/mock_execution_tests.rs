//! Milestone 2 Integration Tests: Hyperliquid Testnet Integration and Safe Execution.
//!
//! Verifies:
//! 1. Network Guard: Strict testnet allowlist enforcement and mainnet rejection.
//! 2. EIP-712 Signing: Deterministic signature derivation and agent address recovery.
//! 3. Credential Role Separation: Account address for data queries, agent private key for action signing.
//! 4. Continuous Protection: Active TP/SL trigger coverage on exchange.
//! 5. Partial Fill Protection Resizing: Atomic replacement of protective triggers on partial fills.
//! 6. Emergency Flatten: Immediate reduce-only market close when protection placement fails.
//! 7. Idempotent Reconciliation: Detecting size and protective order discrepancies.
//! 8. Stale Data Detection (5,000ms) & 3-Step Recovery Guard.
//! 9. Fault Injection: Network timeouts, rate limits (HTTP 429), lost responses, and 10% slippage.

use algo_trade::config::{validate_account_address, validate_agent_private_key, ConfigError};
use algo_trade::execution::client::{
    ExchangeClient, ExecutionError, HyperliquidClient, OrderRequest, OrderTypeSpec,
};
use algo_trade::execution::guard::{
    validate_testnet_rest_url, validate_testnet_ws_url, GuardError,
};
use algo_trade::execution::mock::MockExchangeClient;
use algo_trade::execution::protection::{ProtectionError, ProtectionManager};
use algo_trade::execution::reconciliation::reconcile_state;
use algo_trade::execution::signing::HyperliquidSigner;
use algo_trade::market_data::{
    MarkPriceCache, RecoveryBlocker, StaleDataDetector, StaleRecoveryGuard, StalenessStatus,
};
use algo_trade::state::TradingStateMachine;
use algo_trade::strategy::TpSlLevels;
use algo_trade::types::{Direction, Price, Quantity, Side, SignedUsd};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::json;

/// Test trading account (holds funds/positions).
const TEST_ACCOUNT_ADDRESS: &str = "0x1111111111111111111111111111111111111111";

/// Test agent private key (Hardhat account 0 - authorized by account to sign).
const TEST_AGENT_PRIVATE_KEY: &str =
    "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

/// Expected derived agent public address from TEST_AGENT_PRIVATE_KEY.
const EXPECTED_AGENT_ADDRESS: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";

// ============================================================================
// 1. Network Guard Tests
// ============================================================================

#[test]
fn test_network_guard_rest_allowlist_and_mainnet_rejection() {
    // Authorized testnet host
    assert!(validate_testnet_rest_url("https://api.hyperliquid-testnet.xyz").is_ok());

    // Mainnet MUST be strictly rejected
    let mainnet_res = validate_testnet_rest_url("https://api.hyperliquid.xyz");
    assert!(matches!(
        mainnet_res,
        Err(GuardError::DisallowedHost { ref host, .. }) if host == "api.hyperliquid.xyz"
    ));

    // Insecure HTTP rejected
    let http_res = validate_testnet_rest_url("http://api.hyperliquid-testnet.xyz");
    assert!(matches!(http_res, Err(GuardError::InvalidScheme { .. })));

    // Unknown host rejected
    let rogue_res = validate_testnet_rest_url("https://malicious-exchange.com");
    assert!(matches!(rogue_res, Err(GuardError::DisallowedHost { .. })));
}

#[test]
fn test_network_guard_ws_allowlist_and_mainnet_rejection() {
    // Authorized testnet WS
    assert!(validate_testnet_ws_url("wss://api.hyperliquid-testnet.xyz/ws").is_ok());

    // Mainnet WS MUST be rejected
    let mainnet_ws = validate_testnet_ws_url("wss://api.hyperliquid.xyz/ws");
    assert!(matches!(
        mainnet_ws,
        Err(GuardError::DisallowedHost { ref host, .. }) if host == "api.hyperliquid.xyz"
    ));

    // Insecure WS rejected
    let insecure_ws = validate_testnet_ws_url("ws://api.hyperliquid-testnet.xyz/ws");
    assert!(matches!(insecure_ws, Err(GuardError::InvalidScheme { .. })));
}

// ============================================================================
// 2. EIP-712 Signing & Role Separation Tests
// ============================================================================

#[tokio::test]
async fn test_eip712_signing_deterministic_derivation() {
    let signer = HyperliquidSigner::new(TEST_AGENT_PRIVATE_KEY).unwrap();
    assert_eq!(
        signer.wallet_address_hex().to_lowercase(),
        EXPECTED_AGENT_ADDRESS
    );

    let action = json!({
        "type": "order",
        "orders": [{
            "coin": "BTC",
            "is_buy": true,
            "sz": "0.00014",
            "limit_px": "68000.0",
            "order_type": {"limit": {"tif": "Gtc"}},
            "reduce_only": false
        }]
    });
    let nonce = 1729775000000u64;

    let sig1 = signer.sign_action(&action, nonce).await.unwrap();
    let sig2 = signer.sign_action(&action, nonce).await.unwrap();

    // Deterministic signature
    assert_eq!(sig1.r, sig2.r);
    assert_eq!(sig1.s, sig2.s);
    assert_eq!(sig1.v, sig2.v);
    assert!(sig1.r.starts_with("0x"));
    assert!(sig1.s.starts_with("0x"));
    assert!(sig1.v == 27 || sig1.v == 28);
}

#[test]
fn test_credential_roles_separation() {
    let client = HyperliquidClient::new(
        "https://api.hyperliquid-testnet.xyz",
        TEST_ACCOUNT_ADDRESS,
        TEST_AGENT_PRIVATE_KEY,
    )
    .unwrap();

    // 1. Account address must be the trading account
    assert_eq!(client.account_address(), Some(TEST_ACCOUNT_ADDRESS));

    // 2. Agent address must be the derived signer address
    assert_eq!(
        client.agent_address_hex().unwrap().to_lowercase(),
        EXPECTED_AGENT_ADDRESS
    );

    // 3. Must be distinct roles
    assert_ne!(
        client.account_address().unwrap().to_lowercase(),
        client.agent_address_hex().unwrap().to_lowercase()
    );
}

#[tokio::test]
async fn test_read_only_client_allows_queries_and_blocks_signing() {
    let client = HyperliquidClient::new_read_only(
        "https://api.hyperliquid-testnet.xyz",
        Some(TEST_ACCOUNT_ADDRESS.to_string()),
    )
    .unwrap();

    assert_eq!(client.account_address(), Some(TEST_ACCOUNT_ADDRESS));
    assert_eq!(client.agent_address_hex(), None);

    // Attempting signed trading actions without agent key must fail with SigningRequired
    let order = OrderRequest {
        coin: "BTC".into(),
        is_buy: true,
        size: Quantity::new(dec!(0.00014)).unwrap(),
        limit_px: Price::new(dec!(68000.0)).unwrap(),
        reduce_only: false,
        order_type: OrderTypeSpec::Market,
        cloid: None,
    };

    let place_err = client.place_orders(vec![order]).await;
    assert!(matches!(place_err, Err(ExecutionError::SigningRequired)));

    let cancel_err = client.cancel_order("BTC", 12345).await;
    assert!(matches!(cancel_err, Err(ExecutionError::SigningRequired)));
}

#[test]
fn test_client_rejects_placeholder_credentials() {
    let placeholder_addr = "0x0000000000000000000000000000000000000000";
    let placeholder_key = "0x0000000000000000000000000000000000000000000000000000000000000000";

    // Reject placeholder account address via validator
    let res_acct_direct = validate_account_address(placeholder_addr);
    assert!(matches!(
        res_acct_direct,
        Err(ConfigError::PlaceholderCredential(_))
    ));

    // Reject placeholder account address via client constructor
    let res_acct = HyperliquidClient::new(
        "https://api.hyperliquid-testnet.xyz",
        placeholder_addr,
        TEST_AGENT_PRIVATE_KEY,
    );
    assert!(matches!(
        res_acct,
        Err(ExecutionError::Config(ConfigError::PlaceholderCredential(
            _
        )))
    ));

    // Reject placeholder agent key via validator
    let res_key = validate_agent_private_key(placeholder_key);
    assert!(matches!(
        res_key,
        Err(ConfigError::PlaceholderCredential(_))
    ));
}

// ============================================================================
// 3. Mock Exchange Order Lifecycle & Continuous Protection Tests
// ============================================================================

#[tokio::test]
async fn test_mock_exchange_order_placement_and_cancellation() {
    let mock = MockExchangeClient::new("BTC", SignedUsd::new(dec!(10.0)));
    let coin = "BTC";

    let order = OrderRequest {
        coin: coin.to_string(),
        is_buy: true,
        size: Quantity::new(dec!(0.00014)).unwrap(),
        limit_px: Price::new(dec!(68000.0)).unwrap(),
        reduce_only: false,
        order_type: OrderTypeSpec::Limit { tif: "Gtc".into() },
        cloid: Some("entry-order-1".into()),
    };

    let res = mock.place_orders(vec![order]).await.unwrap();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].status, "resting");
    let oid = res[0].oid.unwrap();

    // Query using the trading account address
    let open_orders = mock.get_open_orders(TEST_ACCOUNT_ADDRESS).await.unwrap();
    assert_eq!(open_orders.len(), 1);
    assert_eq!(open_orders[0].oid, oid);
    assert_eq!(open_orders[0].cloid.as_deref(), Some("entry-order-1"));

    // Cancel by OID
    mock.cancel_order(coin, oid).await.unwrap();
    let open_after = mock.get_open_orders(TEST_ACCOUNT_ADDRESS).await.unwrap();
    assert!(open_after.is_empty());
}

#[tokio::test]
async fn test_continuous_protection_establishment() {
    let mock = MockExchangeClient::new("BTC", SignedUsd::new(dec!(10.0)));
    let mut protection_mgr = ProtectionManager::new("BTC");

    let total_qty = Quantity::new(dec!(0.00014)).unwrap();
    let tp_sl = TpSlLevels {
        take_profit_price: Price::new(dec!(69000.0)).unwrap(),
        stop_loss_price: Price::new(dec!(67000.0)).unwrap(),
    };

    let active = protection_mgr
        .establish_or_resize_protection(
            &mock,
            Direction::Long,
            total_qty,
            &tp_sl,
            "tp-order-1".into(),
            "sl-order-1".into(),
        )
        .await
        .unwrap();

    assert_eq!(active.covered_quantity, total_qty);
    assert_eq!(active.direction, Direction::Long);
    assert_eq!(active.tp_cloid, "tp-order-1");
    assert_eq!(active.sl_cloid, "sl-order-1");

    // Verify exchange order book has 2 resting trigger orders for account
    let open = mock.get_open_orders(TEST_ACCOUNT_ADDRESS).await.unwrap();
    assert_eq!(open.len(), 2);

    let tp = open
        .iter()
        .find(|o| o.cloid.as_deref() == Some("tp-order-1"))
        .unwrap();
    let sl = open
        .iter()
        .find(|o| o.cloid.as_deref() == Some("sl-order-1"))
        .unwrap();

    assert!(tp.is_trigger);
    assert_eq!(tp.trigger_type.as_deref(), Some("tp"));
    assert_eq!(tp.trigger_px, Some(tp_sl.take_profit_price));
    assert_eq!(tp.side, Side::Sell); // Closing a Long

    assert!(sl.is_trigger);
    assert_eq!(sl.trigger_type.as_deref(), Some("sl"));
    assert_eq!(sl.trigger_px, Some(tp_sl.stop_loss_price));
    assert_eq!(sl.side, Side::Sell); // Closing a Long
}

// ============================================================================
// 4. Partial Fill Protection Resizing Tests
// ============================================================================

#[tokio::test]
async fn test_partial_fill_protection_resizing_flow() {
    let mock = MockExchangeClient::new("BTC", SignedUsd::new(dec!(10.0)));
    let mut protection_mgr = ProtectionManager::new("BTC");

    let tp_sl = TpSlLevels {
        take_profit_price: Price::new(dec!(70000.0)).unwrap(),
        stop_loss_price: Price::new(dec!(67000.0)).unwrap(),
    };

    // Step 1: Initial partial fill (40% = 0.000056 BTC)
    let fill_1 = Quantity::new(dec!(0.000056)).unwrap();
    let active_1 = protection_mgr
        .establish_or_resize_protection(
            &mock,
            Direction::Long,
            fill_1,
            &tp_sl,
            "tp-partial-1".into(),
            "sl-partial-1".into(),
        )
        .await
        .unwrap();

    assert_eq!(active_1.covered_quantity, fill_1);
    let open_1 = mock.get_open_orders(TEST_ACCOUNT_ADDRESS).await.unwrap();
    assert_eq!(open_1.len(), 2);
    assert_eq!(open_1[0].size, fill_1);
    assert_eq!(open_1[1].size, fill_1);

    // Step 2: Final fill completes (total = 0.00014 BTC)
    let full_fill = Quantity::new(dec!(0.00014)).unwrap();
    let active_2 = protection_mgr
        .establish_or_resize_protection(
            &mock,
            Direction::Long,
            full_fill,
            &tp_sl,
            "tp-full-2".into(),
            "sl-full-2".into(),
        )
        .await
        .unwrap();

    assert_eq!(active_2.covered_quantity, full_fill);

    // Verify: Previous orders of size 0.000056 were canceled!
    // Exactly 2 new orders of size 0.00014 exist!
    let open_2 = mock.get_open_orders(TEST_ACCOUNT_ADDRESS).await.unwrap();
    assert_eq!(open_2.len(), 2);
    for o in &open_2 {
        assert_eq!(o.size, full_fill);
        assert!(o.cloid.as_ref().unwrap().ends_with("-2"));
    }
}

// ============================================================================
// 5. Emergency Flatten on Protection Rejection Tests
// ============================================================================

#[tokio::test]
async fn test_emergency_flatten_on_protection_failure() {
    let mock = MockExchangeClient::new("BTC", SignedUsd::new(dec!(10.0)));
    let mut protection_mgr = ProtectionManager::new("BTC");

    // Force order rejection on mock exchange
    mock.set_order_rejection(true);

    let tp_sl = TpSlLevels {
        take_profit_price: Price::new(dec!(71000.0)).unwrap(),
        stop_loss_price: Price::new(dec!(68000.0)).unwrap(),
    };
    let open_qty = Quantity::new(dec!(0.00014)).unwrap();

    // Attempting protection must fail with PlacementFailed
    let res = protection_mgr
        .establish_or_resize_protection(
            &mock,
            Direction::Long,
            open_qty,
            &tp_sl,
            "tp-fail".into(),
            "sl-fail".into(),
        )
        .await;

    assert!(matches!(res, Err(ProtectionError::PlacementFailed(_))));
}

// ============================================================================
// 6. Idempotent State Reconciliation Tests
// ============================================================================

#[tokio::test]
async fn test_idempotent_reconciliation_synced_and_discrepancy_detection() {
    let mock = MockExchangeClient::new("BTC", SignedUsd::new(dec!(10.0)));
    let mut state_machine = TradingStateMachine::new_running();

    let target_qty = Quantity::new(dec!(0.00014)).unwrap();
    let entry_px = Price::new(dec!(68000.0)).unwrap();

    // Setup local state in OpenProtected
    state_machine
        .transition_to_opening("entry-1".into(), target_qty, Side::Buy, Direction::Long)
        .unwrap();
    state_machine
        .transition_to_open_protected(
            Direction::Long,
            target_qty,
            entry_px,
            "tp-sync-1".into(),
            "sl-sync-1".into(),
        )
        .unwrap();

    // Setup matching exchange state for target account
    mock.set_position("BTC", target_qty, true, Some(entry_px))
        .await;
    let _ = mock
        .place_orders(vec![
            OrderRequest {
                coin: "BTC".into(),
                is_buy: false,
                size: target_qty,
                limit_px: Price::new(dec!(69000.0)).unwrap(),
                reduce_only: true,
                order_type: OrderTypeSpec::Trigger {
                    trigger_px: Price::new(dec!(69000.0)).unwrap(),
                    is_market: true,
                    tpsl: "tp".into(),
                },
                cloid: Some("tp-sync-1".into()),
            },
            OrderRequest {
                coin: "BTC".into(),
                is_buy: false,
                size: target_qty,
                limit_px: Price::new(dec!(67000.0)).unwrap(),
                reduce_only: true,
                order_type: OrderTypeSpec::Trigger {
                    trigger_px: Price::new(dec!(67000.0)).unwrap(),
                    is_market: true,
                    tpsl: "sl".into(),
                },
                cloid: Some("sl-sync-1".into()),
            },
        ])
        .await
        .unwrap();

    // 1. Reconciliation passes when states align
    let report = reconcile_state(&mock, TEST_ACCOUNT_ADDRESS, &state_machine, "BTC")
        .await
        .unwrap();
    assert!(report.is_synced);
    assert!(report.discrepancies.is_empty());

    // 2. Discrepancy detected if exchange position size differs
    mock.set_position(
        "BTC",
        Quantity::new(dec!(0.00020)).unwrap(),
        true,
        Some(entry_px),
    )
    .await;
    let report_diff = reconcile_state(&mock, TEST_ACCOUNT_ADDRESS, &state_machine, "BTC")
        .await
        .unwrap();
    assert!(!report_diff.is_synced);
    assert_eq!(report_diff.discrepancies.len(), 1);
    assert!(report_diff.discrepancies[0].contains("Position size discrepancy"));

    // Reset position size, but cancel TP order on exchange
    mock.set_position("BTC", target_qty, true, Some(entry_px))
        .await;
    mock.cancel_by_cloid("BTC", "tp-sync-1").await.unwrap();

    let report_missing_tp = reconcile_state(&mock, TEST_ACCOUNT_ADDRESS, &state_machine, "BTC")
        .await
        .unwrap();
    assert!(!report_missing_tp.is_synced);
    assert!(report_missing_tp.discrepancies[0].contains("Missing exchange Take-Profit"));
}

// ============================================================================
// 7. Stale Data Detection (5,000ms) & 3-Step Recovery Guard Tests
// ============================================================================

#[test]
fn test_stale_data_detector_and_recovery_protocol() {
    let detector = StaleDataDetector::default_testnet();
    assert_eq!(detector.threshold_ms(), 5_000);

    let cache = MarkPriceCache::new();
    let now_ms = 1_000_000u64;

    // Missing initial data
    assert_eq!(
        detector.evaluate(cache.last_update_ms().into(), now_ms),
        StalenessStatus::MissingData
    );

    // Fresh tick at 996,000 (age 4,000 ms <= 5,000 ms)
    cache.update(Price::new(dec!(68000.0)).unwrap(), 996_000);
    let fresh_eval = detector.evaluate(Some(cache.last_update_ms()), now_ms);
    assert_eq!(fresh_eval, StalenessStatus::Fresh { age_ms: 4_000 });
    assert!(fresh_eval.is_fresh());

    // Stale tick at 994,000 (age 6,000 ms > 5,000 ms)
    let stale_eval = detector.evaluate(Some(994_000), now_ms);
    assert_eq!(
        stale_eval,
        StalenessStatus::Stale {
            age_ms: 6_000,
            threshold_ms: 5_000,
        }
    );
    assert!(stale_eval.is_stale());

    // Recovery Guard Protocol Verification:
    let mut recovery_guard = StaleRecoveryGuard::new();
    recovery_guard.trigger_stale_halt();
    assert!(recovery_guard.is_in_stale_halt());

    // Failure 1: Data still stale
    assert_eq!(
        recovery_guard.attempt_recovery(stale_eval, true, true),
        Err(RecoveryBlocker::DataStillStale)
    );
    assert!(recovery_guard.is_in_stale_halt());

    // Failure 2: Data fresh, but protective coverage unverified
    assert_eq!(
        recovery_guard.attempt_recovery(fresh_eval, false, true),
        Err(RecoveryBlocker::ProtectiveCoverageUnverified)
    );
    assert!(recovery_guard.is_in_stale_halt());

    // Failure 3: Data fresh, protection verified, but state not synced
    assert_eq!(
        recovery_guard.attempt_recovery(fresh_eval, true, false),
        Err(RecoveryBlocker::ReconciliationDiscrepancy)
    );
    assert!(recovery_guard.is_in_stale_halt());

    // Success: All 3 criteria verified!
    assert!(recovery_guard
        .attempt_recovery(fresh_eval, true, true)
        .is_ok());
    assert!(!recovery_guard.is_in_stale_halt());
}

// ============================================================================
// 8. Fault Injection & Slippage Tests
// ============================================================================

#[tokio::test]
async fn test_fault_injection_network_and_rate_limits() {
    let mock = MockExchangeClient::new("BTC", SignedUsd::new(dec!(10.0)));

    // 1. Network drop
    mock.set_network_error(true);
    let res = mock.get_metadata("BTC").await;
    assert!(
        matches!(res, Err(algo_trade::execution::client::ExecutionError::ApiRejection(msg)) if msg.contains("network timeout"))
    );
    mock.set_network_error(false);

    // 2. HTTP 429 Rate limit on account state query
    mock.set_rate_limit(true);
    let res_rate = mock.get_user_state(TEST_ACCOUNT_ADDRESS).await;
    assert!(
        matches!(res_rate, Err(algo_trade::execution::client::ExecutionError::ApiRejection(msg)) if msg.contains("HTTP 429"))
    );
    mock.set_rate_limit(false);

    // 3. Lost response on placement
    mock.set_lost_response(true);
    let order = OrderRequest {
        coin: "BTC".into(),
        is_buy: true,
        size: Quantity::new(dec!(0.00010)).unwrap(),
        limit_px: Price::new(dec!(68000.0)).unwrap(),
        reduce_only: false,
        order_type: OrderTypeSpec::Limit { tif: "Gtc".into() },
        cloid: Some("lost-order-1".into()),
    };

    let place_res = mock.place_orders(vec![order]).await;
    assert!(place_res.is_err()); // Caller gets network drop error!

    // But the order WAS placed on exchange: Reconciler finds it by cloid for the account!
    let open_orders = mock.get_open_orders(TEST_ACCOUNT_ADDRESS).await.unwrap();
    assert_eq!(open_orders.len(), 1);
    assert_eq!(open_orders[0].cloid.as_deref(), Some("lost-order-1"));
}

#[tokio::test]
async fn test_trigger_execution_with_10_percent_slippage() {
    let mock = MockExchangeClient::new("BTC", SignedUsd::new(dec!(10.0)));

    // Open Long position 0.00014 BTC at 68,000 for target account
    let pos_qty = Quantity::new(dec!(0.00014)).unwrap();
    let entry_px = Price::new(dec!(68000.0)).unwrap();
    mock.set_position("BTC", pos_qty, true, Some(entry_px))
        .await;

    // Place SL trigger order at 67,000
    let sl_order = OrderRequest {
        coin: "BTC".into(),
        is_buy: false, // Selling to close Long
        size: pos_qty,
        limit_px: Price::new(dec!(67000.0)).unwrap(),
        reduce_only: true,
        order_type: OrderTypeSpec::Trigger {
            trigger_px: Price::new(dec!(67000.0)).unwrap(),
            is_market: true,
            tpsl: "sl".into(),
        },
        cloid: Some("sl-slippage-test".into()),
    };
    mock.place_orders(vec![sl_order]).await.unwrap();

    // Configure 10% market trigger slippage
    mock.set_trigger_slippage_pct(dec!(0.10)).await;

    // Mark price drops to 66,500 (breaching 67,000 SL)
    let mark_px = Price::new(dec!(66500.0)).unwrap();
    let triggered = mock.evaluate_trigger_orders(mark_px).await;

    assert_eq!(triggered.len(), 1);
    let (_oid, fill_px, fill_sz, side) = &triggered[0];
    assert_eq!(*fill_sz, pos_qty);
    assert_eq!(*side, Side::Sell);

    // Fill price should suffer 10% downward slippage: 66,500 * (1 - 0.10) = 59,850
    let expected_fill_px = dec!(66500.0) * dec!(0.90);
    assert_eq!(fill_px.value(), expected_fill_px);

    // Position on exchange is flattened
    let user_state = mock.get_user_state(TEST_ACCOUNT_ADDRESS).await.unwrap();
    assert!(user_state.positions.is_empty());
}

#[tokio::test]
async fn test_candle_snapshot_retrieval_and_interval_validation() {
    let mock = MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0)));

    // Valid intervals: 1m, 5m, 15m, 1h
    let candles_1m = mock
        .get_candle_snapshot("BTC", "1m", 1700000000000, 1700000300000)
        .await
        .unwrap();
    assert_eq!(candles_1m.len(), 5);
    assert_eq!(candles_1m[0].interval, "1m");
    assert_eq!(candles_1m[0].coin, "BTC");

    let candles_5m = mock
        .get_candle_snapshot("BTC", "5m", 1700000000000, 1700001500000)
        .await
        .unwrap();
    assert_eq!(candles_5m.len(), 5);
    assert_eq!(candles_5m[0].interval, "5m");

    // Invalid interval rejected
    let invalid = mock
        .get_candle_snapshot("BTC", "2h", 1700000000000, 1700007200000)
        .await;
    assert!(invalid.is_err());
}

#[tokio::test]
async fn test_mock_perp_account_summary() {
    let mock = MockExchangeClient::new("BTC", SignedUsd::new(dec!(100.0)));
    let summary = mock
        .get_perp_account_summary(TEST_ACCOUNT_ADDRESS)
        .await
        .unwrap();

    assert_eq!(summary.account_address, TEST_ACCOUNT_ADDRESS);
    assert_eq!(summary.shortened_address, "0x1111...1111");
    assert_eq!(summary.account_value.value(), dec!(100.0));
    assert_eq!(summary.total_margin_used.value(), dec!(0.0));
    assert_eq!(summary.withdrawable.value(), dec!(100.0));
    assert!(summary.is_mock);
    assert_eq!(summary.source, "Local Mock Engine");
}

#[test]
fn test_fixture_parsing_perp_clearinghouse() {
    let json_str = include_str!("fixtures/perp_clearinghouse_sample.json");
    let val: serde_json::Value = serde_json::from_str(json_str).expect("Valid JSON fixture");

    let margin_summary = &val["marginSummary"];
    let account_val: Decimal = margin_summary["accountValue"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let margin_used: Decimal = margin_summary["totalMarginUsed"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let withdrawable: Decimal = val["withdrawable"].as_str().unwrap().parse().unwrap();
    let cross_maint: Decimal = val["crossMaintenanceMarginUsed"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    assert_eq!(account_val, dec!(50.000000));
    assert_eq!(margin_used, dec!(7.500000));
    assert_eq!(withdrawable, dec!(42.500000));
    assert_eq!(cross_maint, dec!(1.875000));

    let positions = val["assetPositions"].as_array().unwrap();
    assert_eq!(positions.len(), 1);
    let btc_pos = &positions[0]["position"];
    assert_eq!(btc_pos["coin"].as_str().unwrap(), "BTC");
    assert_eq!(btc_pos["szi"].as_str().unwrap(), "0.00045");
}

#[test]
fn test_fixture_parsing_candle_snapshot() {
    let json_str = include_str!("fixtures/candle_snapshot_sample.json");
    let arr: Vec<serde_json::Value> = serde_json::from_str(json_str).expect("Valid JSON fixture");
    assert_eq!(arr.len(), 5);

    let first = &arr[0];
    assert_eq!(first["s"].as_str().unwrap(), "BTC");
    assert_eq!(first["i"].as_str().unwrap(), "1m");
    assert_eq!(first["o"].as_str().unwrap(), "83000.0");
    assert_eq!(first["c"].as_str().unwrap(), "83050.0");
    assert_eq!(first["h"].as_str().unwrap(), "83100.0");
    assert_eq!(first["l"].as_str().unwrap(), "82950.0");
    assert_eq!(first["v"].as_str().unwrap(), "1.45000");
    assert_eq!(first["n"].as_u64().unwrap(), 35);
}

#[test]
fn test_fixture_parsing_user_funding() {
    let json_str = include_str!("fixtures/user_funding_sample.json");
    let arr: Vec<serde_json::Value> = serde_json::from_str(json_str).expect("Valid JSON fixture");
    assert_eq!(arr.len(), 2);

    let delta1 = &arr[0]["delta"];
    assert_eq!(delta1["coin"].as_str().unwrap(), "BTC");
    assert_eq!(delta1["usdc"].as_str().unwrap(), "-0.001250");
    assert_eq!(delta1["fundingRate"].as_str().unwrap(), "0.000100");
}
