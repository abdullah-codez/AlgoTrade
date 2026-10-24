//! Validated configuration module for NORMAL and GREEDY strategy profiles and dynamic market metadata.

use crate::types::{MarginAllocation, SignedUsd};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum ConfigError {
    #[error("Strategy total budget exceeds maximum allowed $10.00: {0}")]
    BudgetExceedsCap(SignedUsd),
    #[error("Initial margin cap exceeds maximum allowed $8.00: {0}")]
    MarginCapExceedsLimit(MarginAllocation),
    #[error(
        "Initial margin cap ({margin}) must be less than total budget ({budget}) to reserve buffer"
    )]
    InsufficientReserveBuffer {
        margin: MarginAllocation,
        budget: SignedUsd,
    },
    #[error("Leverage must be strictly positive: {0}")]
    InvalidLeverage(u32),
    #[error("Take-profit percentage must be positive: {0}")]
    InvalidTakeProfit(Decimal),
    #[error("Stop-loss percentage must be positive: {0}")]
    InvalidStopLoss(Decimal),
    #[error("Hard loss limit must be negative or zero: {0}")]
    InvalidHardLossLimit(SignedUsd),
    #[error("Net profit target must be strictly positive: {0}")]
    InvalidProfitTarget(SignedUsd),
    #[error("Minimum order notional must be at least $10.00: {0}")]
    InvalidMinNotional(SignedUsd),
    #[error("Asset lot decimals must be greater than zero: {0}")]
    InvalidLotDecimals(u32),
    #[error("Missing required environment variable: {0}")]
    MissingEnvVar(String),
    #[error("Configured credential is an unauthorized placeholder: {0}")]
    PlaceholderCredential(String),
    #[error("Invalid Ethereum account address: {0}")]
    InvalidAccountAddress(String),
    #[error("Invalid agent private key: {0}")]
    InvalidAgentPrivateKey(String),
    #[error("Network guard error: {0}")]
    Guard(#[from] crate::execution::guard::GuardError),
}

pub const ENV_HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS: &str = "HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS";
pub const ENV_HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY: &str = "HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY";
pub const ENV_HYPERLIQUID_TESTNET_API_URL: &str = "HYPERLIQUID_TESTNET_API_URL";
pub const ENV_HYPERLIQUID_TESTNET_WS_URL: &str = "HYPERLIQUID_TESTNET_WS_URL";

/// Trading profile selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradingProfile {
    Normal,
    Greedy,
}

/// Margin mode selector, chosen independently of TradingProfile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarginMode {
    Cross,
    Isolated,
}

impl MarginMode {
    pub fn is_cross(&self) -> bool {
        matches!(self, MarginMode::Cross)
    }
}

/// Execution mode selector: automatic execution vs explicit user confirmation per entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionMode {
    Automatic,
    ConfirmationRequired,
}

/// Complete runtime engine configuration combining profile, margin mode, and execution mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineConfig {
    pub profile: ProfileConfig,
    pub margin_mode: MarginMode,
    pub execution_mode: ExecutionMode,
    pub initial_seed: Option<u64>,
}

impl EngineConfig {
    pub fn normal_default() -> Self {
        Self {
            profile: ProfileConfig::normal(),
            margin_mode: MarginMode::Cross,
            execution_mode: ExecutionMode::Automatic,
            initial_seed: Some(42),
        }
    }

    pub fn greedy_default() -> Self {
        Self {
            profile: ProfileConfig::greedy(),
            margin_mode: MarginMode::Cross,
            execution_mode: ExecutionMode::Automatic,
            initial_seed: Some(42),
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.profile.validate()
    }
}

/// Dynamic market metadata fetched from Hyperliquid /info endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketMetadata {
    pub coin: String,
    pub sz_decimals: u32,
    pub max_leverage: u32,
}

impl MarketMetadata {
    pub fn new(
        coin: impl Into<String>,
        sz_decimals: u32,
        max_leverage: u32,
    ) -> Result<Self, ConfigError> {
        if sz_decimals == 0 {
            return Err(ConfigError::InvalidLotDecimals(sz_decimals));
        }
        Ok(Self {
            coin: coin.into(),
            sz_decimals,
            max_leverage,
        })
    }

    /// Standard BTC perp test fixture (szDecimals = 5).
    pub fn btc_fixture() -> Self {
        Self {
            coin: "BTC".to_string(),
            sz_decimals: 5,
            max_leverage: 50,
        }
    }

    /// Calculate minimum size increment as 10^(-sz_decimals).
    pub fn min_size_increment(&self) -> Decimal {
        Decimal::new(1, self.sz_decimals)
    }
}

/// Complete validated profile configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileConfig {
    pub profile: TradingProfile,
    pub total_strategy_budget: SignedUsd,
    pub initial_margin_cap: MarginAllocation,
    pub leverage: u32,
    pub take_profit_pct: Decimal,
    pub stop_loss_pct: Decimal,
    pub take_profit_target_count: u32,
    pub consecutive_losses_limit: u32,
    pub net_session_profit_target: SignedUsd,
    pub hard_session_loss_limit: SignedUsd,
    pub base_taker_fee_rate: Decimal,
    pub base_maker_fee_rate: Decimal,
    pub trailing_stop_enabled: bool,
    pub trailing_stop_trigger_pct: Decimal,
    pub trailing_stop_distance_pct: Decimal,
    pub min_order_notional: SignedUsd,
}

impl ProfileConfig {
    /// Factory for NORMAL Profile:
    /// 5x leverage, $8 initial margin cap, 1.0% TP, 0.5% SL, $0.70 profit target, -$1.00 hard loss limit.
    pub fn normal() -> Self {
        Self {
            profile: TradingProfile::Normal,
            total_strategy_budget: SignedUsd::new(dec!(10.00)),
            initial_margin_cap: MarginAllocation::new(dec!(8.00)).unwrap(),
            leverage: 5,
            take_profit_pct: dec!(0.01), // 1.0%
            stop_loss_pct: dec!(0.005),  // 0.5%
            take_profit_target_count: 2,
            consecutive_losses_limit: 2,
            net_session_profit_target: SignedUsd::new(dec!(0.70)),
            hard_session_loss_limit: SignedUsd::new(dec!(-1.00)),
            base_taker_fee_rate: dec!(0.00045), // 0.045%
            base_maker_fee_rate: dec!(0.00015), // 0.015%
            trailing_stop_enabled: false,
            trailing_stop_trigger_pct: dec!(0.0075),
            trailing_stop_distance_pct: dec!(0.005),
            min_order_notional: SignedUsd::new(dec!(10.00)),
        }
    }

    /// Factory for GREEDY Profile:
    /// 10x leverage, $8 initial margin cap, 1.5% TP, 0.75% SL, $2.00 profit target, -$2.00 hard loss limit, optional trailing stop.
    pub fn greedy() -> Self {
        Self {
            profile: TradingProfile::Greedy,
            total_strategy_budget: SignedUsd::new(dec!(10.00)),
            initial_margin_cap: MarginAllocation::new(dec!(8.00)).unwrap(),
            leverage: 10,
            take_profit_pct: dec!(0.015), // 1.5%
            stop_loss_pct: dec!(0.0075),  // 0.75%
            take_profit_target_count: 2,
            consecutive_losses_limit: 2,
            net_session_profit_target: SignedUsd::new(dec!(2.00)),
            hard_session_loss_limit: SignedUsd::new(dec!(-2.00)),
            base_taker_fee_rate: dec!(0.00045),      // 0.045%
            base_maker_fee_rate: dec!(0.00015),      // 0.015%
            trailing_stop_enabled: false,            // disabled in baseline comparison
            trailing_stop_trigger_pct: dec!(0.0075), // 0.75%
            trailing_stop_distance_pct: dec!(0.005), // 0.5%
            min_order_notional: SignedUsd::new(dec!(10.00)),
        }
    }

    /// Enforces strict architectural and financial invariants.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.total_strategy_budget.value() > dec!(10.00) {
            return Err(ConfigError::BudgetExceedsCap(self.total_strategy_budget));
        }

        if self.initial_margin_cap.value() > dec!(8.00) {
            return Err(ConfigError::MarginCapExceedsLimit(self.initial_margin_cap));
        }

        if self.initial_margin_cap.value() >= self.total_strategy_budget.value() {
            return Err(ConfigError::InsufficientReserveBuffer {
                margin: self.initial_margin_cap,
                budget: self.total_strategy_budget,
            });
        }

        if self.leverage == 0 {
            return Err(ConfigError::InvalidLeverage(self.leverage));
        }

        if self.take_profit_pct <= Decimal::ZERO {
            return Err(ConfigError::InvalidTakeProfit(self.take_profit_pct));
        }

        if self.stop_loss_pct <= Decimal::ZERO {
            return Err(ConfigError::InvalidStopLoss(self.stop_loss_pct));
        }

        if self.hard_session_loss_limit.value() > Decimal::ZERO {
            return Err(ConfigError::InvalidHardLossLimit(
                self.hard_session_loss_limit,
            ));
        }

        if self.net_session_profit_target.value() <= Decimal::ZERO {
            return Err(ConfigError::InvalidProfitTarget(
                self.net_session_profit_target,
            ));
        }

        if self.min_order_notional.value() < dec!(10.00) {
            return Err(ConfigError::InvalidMinNotional(self.min_order_notional));
        }

        Ok(())
    }
}

/// Validated testnet authentication credentials strictly separating trading account from agent signer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestnetAuthConfig {
    /// The account that holds the funds and positions.
    pub account_address: String,
    /// The private key of the authorized API agent wallet used for signing actions.
    pub agent_private_key: String,
}

impl TestnetAuthConfig {
    pub fn new(
        account_address: impl Into<String>,
        agent_private_key: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        let auth = Self {
            account_address: account_address.into(),
            agent_private_key: agent_private_key.into(),
        };
        auth.validate()?;
        Ok(auth)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_account_address(&self.account_address)?;
        validate_agent_private_key(&self.agent_private_key)?;
        Ok(())
    }
}

/// Hyperliquid Testnet environment and credentials configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HyperliquidTestnetConfig {
    pub api_url: String,
    pub ws_url: String,
    pub account_address: Option<String>,
    pub agent_private_key: Option<String>,
}

impl HyperliquidTestnetConfig {
    /// Parse configuration from environment variables.
    /// Does not require credentials for public read-only market data.
    pub fn from_env() -> Result<Self, ConfigError> {
        let api_url = std::env::var(ENV_HYPERLIQUID_TESTNET_API_URL).unwrap_or_else(|_| {
            format!(
                "https://{}",
                crate::execution::guard::ALLOWED_TESTNET_REST_HOST
            )
        });
        let ws_url = std::env::var(ENV_HYPERLIQUID_TESTNET_WS_URL).unwrap_or_else(|_| {
            format!(
                "wss://{}/ws",
                crate::execution::guard::ALLOWED_TESTNET_WS_HOST
            )
        });

        crate::execution::guard::validate_testnet_rest_url(&api_url)?;
        crate::execution::guard::validate_testnet_ws_url(&ws_url)?;

        let account_address = std::env::var(ENV_HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let agent_private_key = std::env::var(ENV_HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        Ok(Self {
            api_url,
            ws_url,
            account_address,
            agent_private_key,
        })
    }

    /// Validates that both trading account address and agent private key are configured and valid.
    pub fn validate_for_trading(&self) -> Result<TestnetAuthConfig, ConfigError> {
        let account_address = self.account_address.as_deref().ok_or_else(|| {
            ConfigError::MissingEnvVar(ENV_HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS.to_string())
        })?;

        let agent_private_key = self.agent_private_key.as_deref().ok_or_else(|| {
            ConfigError::MissingEnvVar(ENV_HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY.to_string())
        })?;

        TestnetAuthConfig::new(account_address, agent_private_key)
    }
}

/// Validates that an account address is a properly formatted 20-byte Ethereum hex address and not a dummy placeholder.
pub fn validate_account_address(addr: &str) -> Result<(), ConfigError> {
    let trimmed = addr.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::InvalidAccountAddress(
            "Account address cannot be empty".to_string(),
        ));
    }

    // Check placeholder patterns
    if trimmed.eq_ignore_ascii_case("0x0000000000000000000000000000000000000000")
        || trimmed.contains("0000000000000000000000000000000000000000")
        || trimmed.contains("YOUR_ACCOUNT")
        || trimmed.contains("PLACEHOLDER")
    {
        return Err(ConfigError::PlaceholderCredential(format!(
            "Account address is an unauthorized placeholder: {}",
            trimmed
        )));
    }

    if !trimmed.starts_with("0x") && !trimmed.starts_with("0X") {
        return Err(ConfigError::InvalidAccountAddress(
            "Account address must start with '0x'".to_string(),
        ));
    }

    if trimmed.len() != 42 {
        return Err(ConfigError::InvalidAccountAddress(format!(
            "Account address must be exactly 42 characters (got {})",
            trimmed.len()
        )));
    }

    if !trimmed[2..].chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ConfigError::InvalidAccountAddress(
            "Account address contains non-hexadecimal characters".to_string(),
        ));
    }

    Ok(())
}

/// Validates that an agent private key is a properly formatted 32-byte hex scalar and not a dummy placeholder.
pub fn validate_agent_private_key(key: &str) -> Result<(), ConfigError> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::InvalidAgentPrivateKey(
            "Agent private key cannot be empty".to_string(),
        ));
    }

    let hex_part = trimmed.trim_start_matches("0x").trim_start_matches("0X");

    // Check placeholder patterns
    if hex_part.chars().all(|c| c == '0')
        || trimmed.contains("YOUR_PRIVATE_KEY")
        || trimmed.contains("PLACEHOLDER")
    {
        return Err(ConfigError::PlaceholderCredential(
            "Agent private key is an unauthorized placeholder (all zeros or template string)"
                .to_string(),
        ));
    }

    if hex_part.len() != 64 {
        return Err(ConfigError::InvalidAgentPrivateKey(format!(
            "Agent private key must be exactly 64 hex characters (got {})",
            hex_part.len()
        )));
    }

    if !hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ConfigError::InvalidAgentPrivateKey(
            "Agent private key contains non-hexadecimal characters".to_string(),
        ));
    }

    use std::str::FromStr;
    ethers::signers::LocalWallet::from_str(hex_part)
        .map_err(|e| ConfigError::InvalidAgentPrivateKey(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_ACCOUNT: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";
    const VALID_AGENT_KEY: &str =
        "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    #[test]
    fn test_normal_profile_validates() {
        let cfg = ProfileConfig::normal();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_greedy_profile_validates() {
        let cfg = ProfileConfig::greedy();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_budget_exceeds_cap_fails() {
        let mut cfg = ProfileConfig::normal();
        cfg.total_strategy_budget = SignedUsd::new(dec!(10.01));
        assert!(matches!(
            cfg.validate(),
            Err(ConfigError::BudgetExceedsCap(_))
        ));
    }

    #[test]
    fn test_margin_cap_exceeds_limit_fails() {
        let mut cfg = ProfileConfig::normal();
        cfg.initial_margin_cap = MarginAllocation::new(dec!(8.50)).unwrap();
        assert!(matches!(
            cfg.validate(),
            Err(ConfigError::MarginCapExceedsLimit(_))
        ));
    }

    #[test]
    fn test_insufficient_buffer_fails() {
        let mut cfg = ProfileConfig::normal();
        cfg.total_strategy_budget = SignedUsd::new(dec!(8.00));
        cfg.initial_margin_cap = MarginAllocation::new(dec!(8.00)).unwrap();
        assert!(matches!(
            cfg.validate(),
            Err(ConfigError::InsufficientReserveBuffer { .. })
        ));
    }

    #[test]
    fn test_validate_account_address_success() {
        assert!(validate_account_address(VALID_ACCOUNT).is_ok());
    }

    #[test]
    fn test_validate_account_address_placeholder_rejection() {
        let zero_addr = "0x0000000000000000000000000000000000000000";
        assert!(matches!(
            validate_account_address(zero_addr),
            Err(ConfigError::PlaceholderCredential(_))
        ));
    }

    #[test]
    fn test_validate_account_address_invalid_format() {
        // Missing 0x
        assert!(matches!(
            validate_account_address("f39fd6e51aad88f6f4ce6ab8827279cfffb92266"),
            Err(ConfigError::InvalidAccountAddress(_))
        ));
        // Too short
        assert!(matches!(
            validate_account_address("0x1234"),
            Err(ConfigError::InvalidAccountAddress(_))
        ));
        // Non-hex
        assert!(matches!(
            validate_account_address("0xZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ"),
            Err(ConfigError::InvalidAccountAddress(_))
        ));
    }

    #[test]
    fn test_validate_agent_private_key_success() {
        assert!(validate_agent_private_key(VALID_AGENT_KEY).is_ok());
    }

    #[test]
    fn test_validate_agent_private_key_placeholder_rejection() {
        let zero_key = "0x0000000000000000000000000000000000000000000000000000000000000000";
        assert!(matches!(
            validate_agent_private_key(zero_key),
            Err(ConfigError::PlaceholderCredential(_))
        ));
    }

    #[test]
    fn test_validate_agent_private_key_invalid_format() {
        // Too short
        assert!(matches!(
            validate_agent_private_key("0x1234"),
            Err(ConfigError::InvalidAgentPrivateKey(_))
        ));
        // Non-hex
        assert!(matches!(
            validate_agent_private_key(
                "0xZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ"
            ),
            Err(ConfigError::InvalidAgentPrivateKey(_))
        ));
    }

    #[test]
    fn test_hyperliquid_testnet_config_validate_for_trading() {
        let mut config = HyperliquidTestnetConfig {
            api_url: "https://api.hyperliquid-testnet.xyz".into(),
            ws_url: "wss://api.hyperliquid-testnet.xyz/ws".into(),
            account_address: None,
            agent_private_key: None,
        };

        // Missing account address
        assert!(matches!(
            config.validate_for_trading(),
            Err(ConfigError::MissingEnvVar(ref var)) if var == ENV_HYPERLIQUID_TESTNET_ACCOUNT_ADDRESS
        ));

        config.account_address = Some(VALID_ACCOUNT.into());

        // Missing agent private key
        assert!(matches!(
            config.validate_for_trading(),
            Err(ConfigError::MissingEnvVar(ref var)) if var == ENV_HYPERLIQUID_TESTNET_AGENT_PRIVATE_KEY
        ));

        config.agent_private_key = Some(VALID_AGENT_KEY.into());

        // All present and valid
        let auth = config.validate_for_trading().unwrap();
        assert_eq!(auth.account_address, VALID_ACCOUNT);
        assert_eq!(auth.agent_private_key, VALID_AGENT_KEY);
    }
}
