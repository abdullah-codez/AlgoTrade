//! Exact host allowlist guard ensuring network connectivity strictly targets the Hyperliquid Testnet.
//! Banned: Any mainnet URL or unexpected hostname.

use thiserror::Error;
use url::Url;

pub const ALLOWED_TESTNET_REST_HOST: &str = "api.hyperliquid-testnet.xyz";
pub const ALLOWED_TESTNET_WS_HOST: &str = "api.hyperliquid-testnet.xyz";

#[derive(Error, Debug, PartialEq, Eq)]
pub enum GuardError {
    #[error("Disallowed network host '{host}'. Only testnet '{allowed}' is authorized")]
    DisallowedHost { host: String, allowed: String },
    #[error("Invalid URL scheme '{scheme}'. Expected '{expected}'")]
    InvalidScheme { scheme: String, expected: String },
    #[error("Failed to parse URL: {0}")]
    UrlParseError(String),
    #[error("Missing host in URL: '{0}'")]
    MissingHost(String),
}

/// Strictly validates that an HTTP REST endpoint points to the authorized Hyperliquid Testnet.
pub fn validate_testnet_rest_url(url_str: &str) -> Result<Url, GuardError> {
    let url = Url::parse(url_str).map_err(|e| GuardError::UrlParseError(e.to_string()))?;

    if url.scheme() != "https" {
        return Err(GuardError::InvalidScheme {
            scheme: url.scheme().to_string(),
            expected: "https".to_string(),
        });
    }

    let host = url
        .host_str()
        .ok_or_else(|| GuardError::MissingHost(url_str.to_string()))?;

    if host != ALLOWED_TESTNET_REST_HOST {
        return Err(GuardError::DisallowedHost {
            host: host.to_string(),
            allowed: ALLOWED_TESTNET_REST_HOST.to_string(),
        });
    }

    Ok(url)
}

/// Strictly validates that a WebSocket endpoint points to the authorized Hyperliquid Testnet.
pub fn validate_testnet_ws_url(url_str: &str) -> Result<Url, GuardError> {
    let url = Url::parse(url_str).map_err(|e| GuardError::UrlParseError(e.to_string()))?;

    if url.scheme() != "wss" {
        return Err(GuardError::InvalidScheme {
            scheme: url.scheme().to_string(),
            expected: "wss".to_string(),
        });
    }

    let host = url
        .host_str()
        .ok_or_else(|| GuardError::MissingHost(url_str.to_string()))?;

    if host != ALLOWED_TESTNET_WS_HOST {
        return Err(GuardError::DisallowedHost {
            host: host.to_string(),
            allowed: ALLOWED_TESTNET_WS_HOST.to_string(),
        });
    }

    Ok(url)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    pub fn test_valid_testnet_rest_url() {
        let valid = "https://api.hyperliquid-testnet.xyz";
        assert!(validate_testnet_rest_url(valid).is_ok());
    }

    #[test]
    pub fn test_valid_testnet_ws_url() {
        let valid = "wss://api.hyperliquid-testnet.xyz/ws";
        assert!(validate_testnet_ws_url(valid).is_ok());
    }

    #[test]
    pub fn test_mainnet_url_rejected() {
        let mainnet_rest = "https://api.hyperliquid.xyz";
        let res = validate_testnet_rest_url(mainnet_rest);
        assert!(matches!(res, Err(GuardError::DisallowedHost { .. })));

        let mainnet_ws = "wss://api.hyperliquid.xyz/ws";
        let res_ws = validate_testnet_ws_url(mainnet_ws);
        assert!(matches!(res_ws, Err(GuardError::DisallowedHost { .. })));
    }

    #[test]
    pub fn test_insecure_scheme_rejected() {
        let insecure_http = "http://api.hyperliquid-testnet.xyz";
        assert!(matches!(
            validate_testnet_rest_url(insecure_http),
            Err(GuardError::InvalidScheme { .. })
        ));

        let insecure_ws = "ws://api.hyperliquid-testnet.xyz/ws";
        assert!(matches!(
            validate_testnet_ws_url(insecure_ws),
            Err(GuardError::InvalidScheme { .. })
        ));
    }
}
