//! Asynchronous exchange client interacting with Hyperliquid Testnet /info and /exchange APIs.

use crate::config::MarketMetadata;
use crate::execution::guard::validate_testnet_rest_url;
use crate::execution::signing::{HyperliquidSignature, HyperliquidSigner, SigningError};
use crate::ledger::FundingRecord;
use crate::types::{Price, Quantity, Side, SignedUsd};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Error, Debug)]
pub enum ExecutionError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Signing error: {0}")]
    Signing(#[from] SigningError),
    #[error("Guard error: {0}")]
    Guard(#[from] crate::execution::guard::GuardError),
    #[error("Config error: {0}")]
    Config(#[from] crate::config::ConfigError),
    #[error("Agent private key required for authenticated actions")]
    SigningRequired,
    #[error("API rejection: {0}")]
    ApiRejection(String),
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("Data parsing error: {0}")]
    DataParsing(String),
}

/// Specifications for submitting an order to Hyperliquid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderTypeSpec {
    Limit {
        tif: String,
    },
    Market,
    Trigger {
        trigger_px: Price,
        is_market: bool,
        tpsl: String, // "tp" or "sl"
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderRequest {
    pub coin: String,
    pub is_buy: bool,
    pub size: Quantity,
    pub limit_px: Price,
    pub reduce_only: bool,
    pub order_type: OrderTypeSpec,
    pub cloid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderPlacementResult {
    pub oid: Option<u64>,
    pub cloid: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExchangeOpenOrder {
    pub coin: String,
    pub side: Side,
    pub limit_px: Price,
    pub size: Quantity,
    pub oid: u64,
    pub cloid: Option<String>,
    pub is_trigger: bool,
    pub trigger_px: Option<Price>,
    pub trigger_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionSummary {
    pub coin: String,
    pub szi: Quantity,
    pub entry_px: Option<Price>,
    pub is_long: bool,
    pub unrealized_pnl: SignedUsd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserStateSummary {
    pub account_value: SignedUsd,
    pub total_margin_used: SignedUsd,
    pub withdrawable: SignedUsd,
    pub positions: Vec<PositionSummary>,
}

/// Canonical candlestick snapshot representing a single timeframe bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandleSnapshot {
    pub open_time_ms: u64,
    pub close_time_ms: u64,
    pub coin: String,
    pub interval: String,
    pub open: Price,
    pub high: Price,
    pub low: Price,
    pub close: Price,
    pub volume: Decimal,
    pub trade_count: u64,
}

/// Dynamic signer status badge for UI and telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignerStatus {
    Active,
    ReadOnly,
    InvalidKeyFormat,
    UnauthorizedOrExpired,
    MissingKey,
    Unconfigured,
}

/// Unified account summary representing spot clearinghouse balances (primarily USDC).
/// Under Hyperliquid account abstraction, account-level collateral and balances reside in spotClearinghouseState.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnifiedAccountSummary {
    pub usdc_total: SignedUsd,
    pub usdc_hold: SignedUsd,
    pub usdc_available: SignedUsd,
}

/// Perpetual clearinghouse summary returned by GET /api/account.
/// Scope Notice: Represents the perpetual account view on Hyperliquid.
/// When unified account mode is active, unified USDC balance is separately reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerpAccountSummary {
    pub account_address: String,
    pub shortened_address: String,
    pub account_value: SignedUsd,
    pub total_margin_used: SignedUsd,
    pub withdrawable: SignedUsd,
    pub cross_maintenance_margin_used: SignedUsd,
    pub positions: Vec<PositionSummary>,
    pub signer_status: SignerStatus,
    pub exchange_time_ms: u64,
    pub source: String,
    pub is_mock: bool,
    pub unified_usdc_total: Option<SignedUsd>,
    pub unified_usdc_hold: Option<SignedUsd>,
    pub unified_usdc_available: Option<SignedUsd>,
    pub unified_account: Option<UnifiedAccountSummary>,
}

/// Format an address dynamically into shortened form (e.g., 0xB251...7b28).
pub fn shorten_address(addr: &str) -> String {
    if addr.len() >= 10 {
        format!("{}...{}", &addr[..6], &addr[addr.len() - 4..])
    } else {
        addr.to_string()
    }
}

/// Validate that a requested candlestick interval is one of the supported resolutions.
pub fn validate_candle_interval(interval: &str) -> Result<(), ExecutionError> {
    match interval {
        "1m" | "5m" | "15m" | "1h" => Ok(()),
        _ => Err(ExecutionError::DataParsing(format!(
            "Unsupported candle interval '{}'. Supported: 1m, 5m, 15m, 1h",
            interval
        ))),
    }
}

/// Trait defining execution operations supported by both live testnet and mock engines.
#[async_trait]
pub trait ExchangeClient: Send + Sync {
    async fn get_metadata(&self, coin: &str) -> Result<MarketMetadata, ExecutionError>;
    async fn get_user_state(&self, address: &str) -> Result<UserStateSummary, ExecutionError>;
    async fn get_spot_clearinghouse_state(
        &self,
        address: &str,
    ) -> Result<UnifiedAccountSummary, ExecutionError>;
    async fn get_perp_account_summary(
        &self,
        address: &str,
    ) -> Result<PerpAccountSummary, ExecutionError>;
    async fn get_candle_snapshot(
        &self,
        coin: &str,
        interval: &str,
        start_time_ms: u64,
        end_time_ms: u64,
    ) -> Result<Vec<CandleSnapshot>, ExecutionError>;
    async fn get_open_orders(
        &self,
        address: &str,
    ) -> Result<Vec<ExchangeOpenOrder>, ExecutionError>;
    async fn get_user_funding(
        &self,
        address: &str,
        start_time_ms: u64,
    ) -> Result<Vec<FundingRecord>, ExecutionError>;
    async fn update_leverage(
        &self,
        coin: &str,
        is_cross: bool,
        leverage: u32,
    ) -> Result<(), ExecutionError>;
    async fn place_orders(
        &self,
        orders: Vec<OrderRequest>,
    ) -> Result<Vec<OrderPlacementResult>, ExecutionError>;
    async fn cancel_order(&self, coin: &str, oid: u64) -> Result<(), ExecutionError>;
    async fn cancel_by_cloid(&self, coin: &str, cloid: &str) -> Result<(), ExecutionError>;
}

/// Real Hyperliquid HTTP client targeting the verified testnet base URL.
pub struct HyperliquidClient {
    http: Client,
    base_url: String,
    account_address: Option<String>,
    signer: Option<HyperliquidSigner>,
    signer_status: Arc<RwLock<SignerStatus>>,
    last_verification_ms: Arc<RwLock<Option<u64>>>,
}

impl HyperliquidClient {
    /// Initialize with target trading account address and authorized agent signing key.
    pub fn new(
        base_url_str: &str,
        account_address: &str,
        agent_private_key_hex: &str,
    ) -> Result<Self, ExecutionError> {
        crate::config::validate_account_address(account_address)?;
        let valid_url = validate_testnet_rest_url(base_url_str)?;
        let signer = HyperliquidSigner::new(agent_private_key_hex)?;

        Ok(Self {
            http: Client::new(),
            base_url: valid_url.to_string().trim_end_matches('/').to_string(),
            account_address: Some(account_address.to_string()),
            signer: Some(signer),
            signer_status: Arc::new(RwLock::new(SignerStatus::UnauthorizedOrExpired)),
            last_verification_ms: Arc::new(RwLock::new(None)),
        })
    }

    /// Initialize read-only client for public market data and account observation without private key.
    pub fn new_read_only(
        base_url_str: &str,
        account_address: Option<String>,
    ) -> Result<Self, ExecutionError> {
        Self::new_read_only_with_status(base_url_str, account_address, SignerStatus::ReadOnly)
    }

    /// Initialize read-only client with explicit diagnosed signer status.
    pub fn new_read_only_with_status(
        base_url_str: &str,
        account_address: Option<String>,
        signer_status: SignerStatus,
    ) -> Result<Self, ExecutionError> {
        let valid_url = validate_testnet_rest_url(base_url_str)?;
        if let Some(ref addr) = account_address {
            crate::config::validate_account_address(addr)?;
        }

        Ok(Self {
            http: Client::new(),
            base_url: valid_url.to_string().trim_end_matches('/').to_string(),
            account_address,
            signer: None,
            signer_status: Arc::new(RwLock::new(signer_status)),
            last_verification_ms: Arc::new(RwLock::new(None)),
        })
    }

    /// Pure evaluation function to check if the agent address is listed as actively authorized.
    ///
    /// Requires verified matching address and strictly positive validUntil in the future (> now_ms).
    /// Missing or malformed validUntil cannot become authorized.
    pub fn evaluate_extra_agents_authorization(
        data: &serde_json::Value,
        agent_addr: &str,
        now_ms: u64,
    ) -> bool {
        if let Some(arr) = data.as_array() {
            for entry in arr {
                if let Some(addr) = entry["address"].as_str() {
                    if addr.eq_ignore_ascii_case(agent_addr) {
                        // Strictly require validUntil to be a valid integer strictly in the future (> now_ms).
                        // Missing or malformed validUntil cannot become Active through unwrap_or(0).
                        if let Some(valid_until) = entry["validUntil"].as_u64() {
                            if valid_until > now_ms {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

    /// Query exchange /info to verify whether the agent wallet is actively authorized on Hyperliquid.
    ///
    /// Tracks verification freshness. Authorization-query failures must not allow indefinitely stale Active status.
    pub async fn verify_signer_authorization(&self) -> SignerStatus {
        let account = match self.account_address.as_deref() {
            Some(a) => a,
            None => {
                let mut st = self.signer_status.write().await;
                *st = SignerStatus::MissingKey;
                return SignerStatus::MissingKey;
            }
        };

        let signer = match self.signer.as_ref() {
            Some(s) => s,
            None => {
                return *self.signer_status.read().await;
            }
        };

        let agent_addr = signer.wallet_address_hex();
        let payload = json!({"type": "extraAgents", "user": account});
        let now_ms = Self::current_timestamp_ms();

        match self.post_info(payload).await {
            Ok(data) => {
                let authorized =
                    Self::evaluate_extra_agents_authorization(&data, &agent_addr, now_ms);
                let status = if authorized {
                    SignerStatus::Active
                } else {
                    SignerStatus::UnauthorizedOrExpired
                };
                *self.signer_status.write().await = status;
                *self.last_verification_ms.write().await = Some(now_ms);
                status
            }
            Err(e) => {
                tracing::warn!("Failed to query extraAgents: {}.", e);
                let last_ms = *self.last_verification_ms.read().await;
                const MAX_VERIFICATION_STALENESS_MS: u64 = 60_000;
                let is_fresh = match last_ms {
                    Some(t) => now_ms.saturating_sub(t) <= MAX_VERIFICATION_STALENESS_MS,
                    None => false,
                };
                let mut st = self.signer_status.write().await;
                if *st == SignerStatus::Active && !is_fresh {
                    tracing::warn!(
                        "Signer status was Active but verification query failed and cached status is stale (>60s). Downgrading to UnauthorizedOrExpired."
                    );
                    *st = SignerStatus::UnauthorizedOrExpired;
                }
                *st
            }
        }
    }

    /// Return current signer status without making an asynchronous network call.
    pub async fn current_signer_status(&self) -> SignerStatus {
        *self.signer_status.read().await
    }

    /// The target trading account address (used for position/balance/order queries).
    pub fn account_address(&self) -> Option<&str> {
        self.account_address.as_deref()
    }

    /// The authorized agent wallet address derived from the agent private key (used for signing).
    pub fn agent_address_hex(&self) -> Option<String> {
        self.signer.as_ref().map(|s| s.wallet_address_hex())
    }

    fn get_signer(&self) -> Result<&HyperliquidSigner, ExecutionError> {
        self.signer.as_ref().ok_or(ExecutionError::SigningRequired)
    }

    fn current_timestamp_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    async fn post_info(
        &self,
        payload: serde_json::Value,
    ) -> Result<serde_json::Value, ExecutionError> {
        let url = format!("{}/info", self.base_url);
        let res = self.http.post(&url).json(&payload).send().await?;

        if !res.status().is_success() {
            let err_text = res.text().await.unwrap_or_default();
            return Err(ExecutionError::ApiRejection(format!(
                "Info query failed: {}",
                err_text
            )));
        }

        let json_val = res.json::<serde_json::Value>().await?;
        Ok(json_val)
    }

    async fn post_exchange(
        &self,
        action: serde_json::Value,
        nonce: u64,
        signature: HyperliquidSignature,
    ) -> Result<serde_json::Value, ExecutionError> {
        let url = format!("{}/exchange", self.base_url);
        let payload = json!({
            "action": action,
            "nonce": nonce,
            "signature": signature
        });

        let res = self.http.post(&url).json(&payload).send().await?;
        if !res.status().is_success() {
            let err_text = res.text().await.unwrap_or_default();
            return Err(ExecutionError::ApiRejection(format!(
                "Exchange action failed: {}",
                err_text
            )));
        }

        let json_val = res.json::<serde_json::Value>().await?;
        Ok(json_val)
    }
}

/// Helper to strictly parse a required decimal string from a JSON value.
/// Returns DataParsing error if field is missing or not a valid decimal string.
/// Preserves legitimate zero values (e.g. "0", "0.0").
pub fn parse_required_decimal(
    parent: &serde_json::Value,
    field_name: &str,
) -> Result<Decimal, ExecutionError> {
    let val_str = parent
        .get(field_name)
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            ExecutionError::DataParsing(format!(
                "Missing required clearinghouse field '{}'",
                field_name
            ))
        })?;

    val_str.parse::<Decimal>().map_err(|e| {
        ExecutionError::DataParsing(format!(
            "Malformed decimal in required clearinghouse field '{}': '{}' ({})",
            field_name, val_str, e
        ))
    })
}

impl HyperliquidClient {
    /// Pure parsing method for UserStateSummary from clearinghouseState JSON.
    pub fn parse_user_state(data: &serde_json::Value) -> Result<UserStateSummary, ExecutionError> {
        let margin_summary = data.get("marginSummary").ok_or_else(|| {
            ExecutionError::DataParsing(
                "Missing required 'marginSummary' object in clearinghouse response".into(),
            )
        })?;

        let account_val = parse_required_decimal(margin_summary, "accountValue")?;
        let margin_used = parse_required_decimal(margin_summary, "totalMarginUsed")?;
        let withdrawable = parse_required_decimal(data, "withdrawable")?;

        let mut positions = Vec::new();
        if let Some(pos_arr) = data["assetPositions"].as_array() {
            for item in pos_arr {
                let pos = &item["position"];
                if let Some(coin) = pos["coin"].as_str() {
                    let szi_dec = pos["szi"]
                        .as_str()
                        .and_then(|s| s.parse::<Decimal>().ok())
                        .unwrap_or(Decimal::ZERO);

                    if !szi_dec.is_zero() {
                        let is_long = szi_dec > Decimal::ZERO;
                        let abs_qty =
                            Quantity::new(szi_dec.abs()).unwrap_or_else(|_| Quantity::zero());
                        let entry_px = pos["entryPx"]
                            .as_str()
                            .and_then(|s| s.parse::<Decimal>().ok())
                            .and_then(|d| Price::new(d).ok());
                        let unrealized_pnl = pos["unrealizedPnl"]
                            .as_str()
                            .and_then(|s| s.parse::<Decimal>().ok())
                            .map(SignedUsd::new)
                            .unwrap_or_else(SignedUsd::zero);

                        positions.push(PositionSummary {
                            coin: coin.to_string(),
                            szi: abs_qty,
                            entry_px,
                            is_long,
                            unrealized_pnl,
                        });
                    }
                }
            }
        }

        Ok(UserStateSummary {
            account_value: SignedUsd::new(account_val),
            total_margin_used: SignedUsd::new(margin_used),
            withdrawable: SignedUsd::new(withdrawable),
            positions,
        })
    }

    /// Pure parsing method for PerpAccountSummary from clearinghouseState JSON.
    pub fn parse_clearinghouse_summary(
        data: &serde_json::Value,
        address: &str,
        signer_status: SignerStatus,
        source: &str,
        is_mock: bool,
    ) -> Result<PerpAccountSummary, ExecutionError> {
        let margin_summary = data.get("marginSummary").ok_or_else(|| {
            ExecutionError::DataParsing(
                "Missing required 'marginSummary' object in clearinghouse response".into(),
            )
        })?;

        let account_val = parse_required_decimal(margin_summary, "accountValue")?;
        let margin_used = parse_required_decimal(margin_summary, "totalMarginUsed")?;
        let withdrawable = parse_required_decimal(data, "withdrawable")?;
        let cross_maint = parse_required_decimal(data, "crossMaintenanceMarginUsed")?;
        let time_ms = data["time"].as_u64().unwrap_or(0);

        let mut positions = Vec::new();
        if let Some(pos_arr) = data["assetPositions"].as_array() {
            for item in pos_arr {
                let pos = &item["position"];
                if let Some(coin) = pos["coin"].as_str() {
                    let szi_dec = pos["szi"]
                        .as_str()
                        .and_then(|s| s.parse::<Decimal>().ok())
                        .unwrap_or(Decimal::ZERO);

                    if !szi_dec.is_zero() {
                        let is_long = szi_dec > Decimal::ZERO;
                        let abs_qty =
                            Quantity::new(szi_dec.abs()).unwrap_or_else(|_| Quantity::zero());
                        let entry_px = pos["entryPx"]
                            .as_str()
                            .and_then(|s| s.parse::<Decimal>().ok())
                            .and_then(|d| Price::new(d).ok());
                        let unrealized_pnl = pos["unrealizedPnl"]
                            .as_str()
                            .and_then(|s| s.parse::<Decimal>().ok())
                            .map(SignedUsd::new)
                            .unwrap_or_else(SignedUsd::zero);

                        positions.push(PositionSummary {
                            coin: coin.to_string(),
                            szi: abs_qty,
                            entry_px,
                            is_long,
                            unrealized_pnl,
                        });
                    }
                }
            }
        }

        Ok(PerpAccountSummary {
            account_address: address.to_string(),
            shortened_address: shorten_address(address),
            account_value: SignedUsd::new(account_val),
            total_margin_used: SignedUsd::new(margin_used),
            withdrawable: SignedUsd::new(withdrawable),
            cross_maintenance_margin_used: SignedUsd::new(cross_maint),
            positions,
            signer_status,
            exchange_time_ms: time_ms,
            source: source.to_string(),
            is_mock,
            unified_usdc_total: None,
            unified_usdc_hold: None,
            unified_usdc_available: None,
            unified_account: None,
        })
    }

    /// Pure parsing method for UnifiedAccountSummary from spotClearinghouseState JSON.
    ///
    /// Preserves exact total and hold without floating-point arithmetic.
    /// Distinguishes genuine zero from missing fields or parsing failures.
    pub fn parse_spot_clearinghouse_state(
        data: &serde_json::Value,
    ) -> Result<UnifiedAccountSummary, ExecutionError> {
        let balances_arr = data
            .get("balances")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                ExecutionError::DataParsing(
                    "Missing required 'balances' array in spotClearinghouseState response".into(),
                )
            })?;

        let usdc_entry = balances_arr
            .iter()
            .find(|b| {
                b.get("coin")
                    .and_then(|c| c.as_str())
                    .map(|s| s.eq_ignore_ascii_case("USDC"))
                    .unwrap_or(false)
            })
            .ok_or_else(|| {
                ExecutionError::DataParsing(
                    "Missing 'USDC' balance entry in spotClearinghouseState".into(),
                )
            })?;

        let total_dec = parse_required_decimal(usdc_entry, "total")?;
        let hold_dec = parse_required_decimal(usdc_entry, "hold")?;
        let avail_dec = (total_dec - hold_dec).max(Decimal::ZERO);

        Ok(UnifiedAccountSummary {
            usdc_total: SignedUsd::new(total_dec),
            usdc_hold: SignedUsd::new(hold_dec),
            usdc_available: SignedUsd::new(avail_dec),
        })
    }
}

#[async_trait]
impl ExchangeClient for HyperliquidClient {
    async fn get_metadata(&self, target_coin: &str) -> Result<MarketMetadata, ExecutionError> {
        let payload = json!({"type": "metaAndAssetCtxs"});
        let data = self.post_info(payload).await?;

        let universe = data[0]["universe"]
            .as_array()
            .ok_or_else(|| ExecutionError::DataParsing("Missing universe in meta".into()))?;

        for coin_meta in universe {
            if let Some(name) = coin_meta["name"].as_str() {
                if name.eq_ignore_ascii_case(target_coin) {
                    let sz_decimals = coin_meta["szDecimals"].as_u64().unwrap_or(5) as u32;
                    let max_leverage = coin_meta["maxLeverage"].as_u64().unwrap_or(50) as u32;
                    return Ok(MarketMetadata {
                        coin: target_coin.to_string(),
                        sz_decimals,
                        max_leverage,
                    });
                }
            }
        }

        Err(ExecutionError::DataParsing(format!(
            "Coin {} not found in metadata",
            target_coin
        )))
    }

    async fn get_user_state(&self, address: &str) -> Result<UserStateSummary, ExecutionError> {
        let payload = json!({"type": "clearinghouseState", "user": address});
        let data = self.post_info(payload).await?;
        Self::parse_user_state(&data)
    }

    async fn get_spot_clearinghouse_state(
        &self,
        address: &str,
    ) -> Result<UnifiedAccountSummary, ExecutionError> {
        let payload = json!({"type": "spotClearinghouseState", "user": address});
        let data = self.post_info(payload).await?;
        Self::parse_spot_clearinghouse_state(&data)
    }

    async fn get_perp_account_summary(
        &self,
        address: &str,
    ) -> Result<PerpAccountSummary, ExecutionError> {
        let perp_payload = json!({"type": "clearinghouseState", "user": address});
        let spot_payload = json!({"type": "spotClearinghouseState", "user": address});

        let (perp_data, spot_data) =
            tokio::try_join!(self.post_info(perp_payload), self.post_info(spot_payload))?;

        let signer_status = if self.signer.is_some() {
            self.verify_signer_authorization().await
        } else {
            *self.signer_status.read().await
        };

        let mut summary = Self::parse_clearinghouse_summary(
            &perp_data,
            address,
            signer_status,
            "Hyperliquid Testnet",
            false,
        )?;

        let spot_summary = Self::parse_spot_clearinghouse_state(&spot_data)?;
        summary.unified_usdc_total = Some(spot_summary.usdc_total);
        summary.unified_usdc_hold = Some(spot_summary.usdc_hold);
        summary.unified_usdc_available = Some(spot_summary.usdc_available);
        summary.unified_account = Some(spot_summary);

        Ok(summary)
    }

    async fn get_candle_snapshot(
        &self,
        coin: &str,
        interval: &str,
        start_time_ms: u64,
        end_time_ms: u64,
    ) -> Result<Vec<CandleSnapshot>, ExecutionError> {
        validate_candle_interval(interval)?;

        let payload = json!({
            "type": "candleSnapshot",
            "req": {
                "coin": coin,
                "interval": interval,
                "startTime": start_time_ms,
                "endTime": end_time_ms
            }
        });
        let data = self.post_info(payload).await?;

        let mut candles = Vec::new();
        if let Some(arr) = data.as_array() {
            for c in arr {
                let open_time_ms = c["t"].as_u64().unwrap_or(0);
                let close_time_ms = c["T"].as_u64().unwrap_or(0);
                let coin_sym = c["s"].as_str().unwrap_or(coin).to_string();
                let intv = c["i"].as_str().unwrap_or(interval).to_string();
                let open = Price::new(
                    c["o"]
                        .as_str()
                        .and_then(|s| s.parse::<Decimal>().ok())
                        .unwrap_or(Decimal::ZERO),
                )
                .unwrap_or_else(|_| Price::new(Decimal::ZERO).unwrap());
                let close = Price::new(
                    c["c"]
                        .as_str()
                        .and_then(|s| s.parse::<Decimal>().ok())
                        .unwrap_or(Decimal::ZERO),
                )
                .unwrap_or_else(|_| Price::new(Decimal::ZERO).unwrap());
                let high = Price::new(
                    c["h"]
                        .as_str()
                        .and_then(|s| s.parse::<Decimal>().ok())
                        .unwrap_or(Decimal::ZERO),
                )
                .unwrap_or_else(|_| Price::new(Decimal::ZERO).unwrap());
                let low = Price::new(
                    c["l"]
                        .as_str()
                        .and_then(|s| s.parse::<Decimal>().ok())
                        .unwrap_or(Decimal::ZERO),
                )
                .unwrap_or_else(|_| Price::new(Decimal::ZERO).unwrap());
                let volume = c["v"]
                    .as_str()
                    .and_then(|s| s.parse::<Decimal>().ok())
                    .unwrap_or(Decimal::ZERO);
                let trade_count = c["n"].as_u64().unwrap_or(0);

                candles.push(CandleSnapshot {
                    open_time_ms,
                    close_time_ms,
                    coin: coin_sym,
                    interval: intv,
                    open,
                    high,
                    low,
                    close,
                    volume,
                    trade_count,
                });
            }
        }

        candles.sort_by_key(|c| c.open_time_ms);
        candles.dedup_by_key(|c| c.open_time_ms);
        Ok(candles)
    }

    async fn get_open_orders(
        &self,
        address: &str,
    ) -> Result<Vec<ExchangeOpenOrder>, ExecutionError> {
        let payload = json!({"type": "frontendOpenOrders", "user": address});
        let data = self.post_info(payload).await?;

        let mut orders = Vec::new();
        if let Some(arr) = data.as_array() {
            for o in arr {
                let coin = o["coin"].as_str().unwrap_or("BTC").to_string();
                let side_str = o["side"].as_str().unwrap_or("B");
                let side = if side_str == "B" {
                    Side::Buy
                } else {
                    Side::Sell
                };
                let limit_px = Price::new(
                    o["limitPx"]
                        .as_str()
                        .and_then(|s| s.parse::<Decimal>().ok())
                        .unwrap_or(Decimal::ZERO),
                )
                .unwrap_or(Price::new(Decimal::ZERO).unwrap());
                let size = Quantity::new(
                    o["sz"]
                        .as_str()
                        .and_then(|s| s.parse::<Decimal>().ok())
                        .unwrap_or(Decimal::ZERO),
                )
                .unwrap_or(Quantity::zero());
                let oid = o["oid"].as_u64().unwrap_or(0);
                let cloid = o["cloid"].as_str().map(|s| s.to_string());
                let is_trigger = o["isTrigger"].as_bool().unwrap_or(false);
                let trigger_px = o["triggerPx"]
                    .as_str()
                    .and_then(|s| s.parse::<Decimal>().ok())
                    .and_then(|d| Price::new(d).ok());
                let trigger_type = o["triggerCondition"].as_str().map(|s| s.to_string());

                orders.push(ExchangeOpenOrder {
                    coin,
                    side,
                    limit_px,
                    size,
                    oid,
                    cloid,
                    is_trigger,
                    trigger_px,
                    trigger_type,
                });
            }
        }

        Ok(orders)
    }

    async fn get_user_funding(
        &self,
        address: &str,
        start_time_ms: u64,
    ) -> Result<Vec<FundingRecord>, ExecutionError> {
        let payload = json!({
            "type": "userFunding",
            "user": address,
            "startTime": start_time_ms
        });
        let data = self.post_info(payload).await?;

        let mut records = Vec::new();
        if let Some(arr) = data.as_array() {
            for item in arr {
                let delta = &item["delta"];
                let coin = delta["coin"].as_str().unwrap_or("BTC").to_string();
                let rate = delta["fundingRate"]
                    .as_str()
                    .and_then(|s| s.parse::<Decimal>().ok())
                    .unwrap_or(Decimal::ZERO);
                let szi = delta["szi"]
                    .as_str()
                    .and_then(|s| s.parse::<Decimal>().ok())
                    .unwrap_or(Decimal::ZERO)
                    .abs();
                let payment_val = delta["usdc"]
                    .as_str()
                    .and_then(|s| s.parse::<Decimal>().ok())
                    .unwrap_or(Decimal::ZERO);

                let time_ms = item["time"].as_i64().unwrap_or(0);
                let timestamp = DateTime::from_timestamp_millis(time_ms).unwrap_or_else(Utc::now);

                records.push(FundingRecord {
                    timestamp,
                    coin,
                    rate,
                    position_size: Quantity::new(szi).unwrap_or(Quantity::zero()),
                    payment: SignedUsd::new(payment_val),
                });
            }
        }

        Ok(records)
    }

    async fn update_leverage(
        &self,
        coin: &str,
        is_cross: bool,
        leverage: u32,
    ) -> Result<(), ExecutionError> {
        let action = json!({
            "type": "updateLeverage",
            "coin": coin,
            "isCross": is_cross,
            "leverage": leverage
        });
        let nonce = Self::current_timestamp_ms();
        let sig = self.get_signer()?.sign_action(&action, nonce).await?;

        let res = self.post_exchange(action, nonce, sig).await?;
        if res["status"] == "ok" {
            Ok(())
        } else {
            Err(ExecutionError::ApiRejection(format!(
                "Failed to update leverage: {:?}",
                res
            )))
        }
    }

    async fn place_orders(
        &self,
        orders: Vec<OrderRequest>,
    ) -> Result<Vec<OrderPlacementResult>, ExecutionError> {
        let mut wire_orders = Vec::new();
        for o in &orders {
            let order_type_json = match &o.order_type {
                OrderTypeSpec::Limit { tif } => json!({"limit": {"tif": tif}}),
                OrderTypeSpec::Market => json!({"limit": {"tif": "Ioc"}}),
                OrderTypeSpec::Trigger {
                    trigger_px,
                    is_market,
                    tpsl,
                } => json!({
                    "trigger": {
                        "triggerPx": trigger_px.to_string(),
                        "isMarket": is_market,
                        "tpsl": tpsl
                    }
                }),
            };

            let wire_order = json!({
                "coin": o.coin,
                "is_buy": o.is_buy,
                "sz": o.size.to_string(),
                "limit_px": o.limit_px.to_string(),
                "order_type": order_type_json,
                "reduce_only": o.reduce_only,
                "cloid": o.cloid
            });
            wire_orders.push(wire_order);
        }

        let action = json!({
            "type": "order",
            "orders": wire_orders,
            "grouping": "na"
        });

        let nonce = Self::current_timestamp_ms();
        let sig = self.get_signer()?.sign_action(&action, nonce).await?;
        let res = self.post_exchange(action, nonce, sig).await?;

        let mut results = Vec::new();
        if let Some(statuses) = res["response"]["data"]["statuses"].as_array() {
            for (idx, st) in statuses.iter().enumerate() {
                let cloid = orders.get(idx).and_then(|o| o.cloid.clone());
                if let Some(oid) = st["resting"]["oid"].as_u64() {
                    results.push(OrderPlacementResult {
                        oid: Some(oid),
                        cloid,
                        status: "resting".into(),
                    });
                } else if let Some(oid) = st["filled"]["oid"].as_u64() {
                    results.push(OrderPlacementResult {
                        oid: Some(oid),
                        cloid,
                        status: "filled".into(),
                    });
                } else if let Some(err) = st["error"].as_str() {
                    results.push(OrderPlacementResult {
                        oid: None,
                        cloid,
                        status: format!("error: {}", err),
                    });
                }
            }
        }

        Ok(results)
    }

    async fn cancel_order(&self, coin: &str, oid: u64) -> Result<(), ExecutionError> {
        let action = json!({
            "type": "cancel",
            "cancels": [{
                "coin": coin,
                "oid": oid
            }]
        });
        let nonce = Self::current_timestamp_ms();
        let sig = self.get_signer()?.sign_action(&action, nonce).await?;

        let res = self.post_exchange(action, nonce, sig).await?;
        if res["status"] == "ok" {
            Ok(())
        } else {
            Err(ExecutionError::ApiRejection(format!(
                "Failed to cancel order: {:?}",
                res
            )))
        }
    }

    async fn cancel_by_cloid(&self, coin: &str, cloid: &str) -> Result<(), ExecutionError> {
        let action = json!({
            "type": "cancelByCloid",
            "cancels": [{
                "coin": coin,
                "cloid": cloid
            }]
        });
        let nonce = Self::current_timestamp_ms();
        let sig = self.get_signer()?.sign_action(&action, nonce).await?;

        let res = self.post_exchange(action, nonce, sig).await?;
        if res["status"] == "ok" {
            Ok(())
        } else {
            Err(ExecutionError::ApiRejection(format!(
                "Failed to cancel order by cloid: {:?}",
                res
            )))
        }
    }
}
