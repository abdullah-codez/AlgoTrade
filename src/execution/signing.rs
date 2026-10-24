//! EIP-712 transaction signing for Hyperliquid Testnet actions using LocalWallet.
//! Compliant with Hyperliquid L1 EIP-712 "Agent" signing specifications.

use ethers::prelude::*;
use ethers::types::transaction::eip712::{EIP712Domain, Eip712, Eip712Error};
use ethers::utils::keccak256;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

pub const HYPERLIQUID_TESTNET_CHAIN_ID: u64 = 1337;
pub const VERIFYING_CONTRACT: Address = Address::zero();

#[derive(Error, Debug)]
pub enum SigningError {
    #[error("Failed to parse private key: {0}")]
    InvalidPrivateKey(String),
    #[error("EIP-712 signing error: {0}")]
    SigningFailed(String),
    #[error("Failed to serialize action: {0}")]
    SerializationError(String),
}

/// EIP-712 Signature format required by Hyperliquid /exchange endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HyperliquidSignature {
    pub r: String,
    pub s: String,
    pub v: u8,
}

/// EIP-712 Agent payload matching Hyperliquid L1 domain:
/// Agent(address source,bytes32 connectionId)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub source: Address,
    pub connection_id: [u8; 32],
}

impl Eip712 for AgentMessage {
    type Error = Eip712Error;

    fn domain(&self) -> Result<EIP712Domain, Self::Error> {
        Ok(EIP712Domain {
            name: Some("Exchange".to_string()),
            version: Some("1".to_string()),
            chain_id: Some(U256::from(HYPERLIQUID_TESTNET_CHAIN_ID)),
            verifying_contract: Some(VERIFYING_CONTRACT),
            salt: None,
        })
    }

    fn type_hash() -> Result<[u8; 32], Self::Error> {
        // keccak256("Agent(address source,bytes32 connectionId)")
        Ok(keccak256(
            "Agent(address source,bytes32 connectionId)".as_bytes(),
        ))
    }

    fn struct_hash(&self) -> Result<[u8; 32], Self::Error> {
        let mut encoded = Vec::with_capacity(96);
        encoded.extend_from_slice(&Self::type_hash()?);
        // address source padded to 32 bytes
        let mut source_padded = [0u8; 32];
        source_padded[12..32].copy_from_slice(self.source.as_bytes());
        encoded.extend_from_slice(&source_padded);
        // bytes32 connectionId
        encoded.extend_from_slice(&self.connection_id);

        Ok(keccak256(&encoded))
    }
}

/// Wrapper for local Ethereum wallet providing EIP-712 signing for Hyperliquid actions.
pub struct HyperliquidSigner {
    wallet: LocalWallet,
    address: Address,
}

impl HyperliquidSigner {
    /// Initialize with hex-encoded 32-byte Ethereum private key.
    pub fn new(private_key_hex: &str) -> Result<Self, SigningError> {
        let trimmed = private_key_hex.trim().trim_start_matches("0x");
        let wallet = LocalWallet::from_str(trimmed)
            .map_err(|e| SigningError::InvalidPrivateKey(e.to_string()))?
            .with_chain_id(HYPERLIQUID_TESTNET_CHAIN_ID);
        let address = wallet.address();

        Ok(Self { wallet, address })
    }

    pub fn address(&self) -> Address {
        self.address
    }

    pub fn wallet_address_hex(&self) -> String {
        format!("{:#x}", self.address)
    }

    /// Sign an arbitrary action payload using the EIP-712 Agent specification.
    /// connectionId is computed as keccak256(action_json_bytes + nonce_u64_be_bytes).
    pub async fn sign_action(
        &self,
        action: &serde_json::Value,
        nonce: u64,
    ) -> Result<HyperliquidSignature, SigningError> {
        // Hash action + nonce
        let action_bytes = serde_json::to_vec(action)
            .map_err(|e| SigningError::SerializationError(e.to_string()))?;
        let mut hasher_input = action_bytes;
        hasher_input.extend_from_slice(&nonce.to_be_bytes());
        let connection_id = keccak256(&hasher_input);

        let agent_msg = AgentMessage {
            source: self.address,
            connection_id,
        };

        let digest = agent_msg
            .encode_eip712()
            .map_err(|e| SigningError::SigningFailed(e.to_string()))?;

        let sig = self
            .wallet
            .sign_hash(H256::from(digest))
            .map_err(|e| SigningError::SigningFailed(e.to_string()))?;

        Ok(HyperliquidSignature {
            r: format!("0x{:x}", sig.r),
            s: format!("0x{:x}", sig.s),
            v: sig.v as u8,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Known test private key (Hardhat account 0: 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80)
    const TEST_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const EXPECTED_ADDR: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";

    #[test]
    fn test_signer_address_derivation() {
        let signer = HyperliquidSigner::new(TEST_KEY).unwrap();
        assert_eq!(signer.wallet_address_hex().to_lowercase(), EXPECTED_ADDR);
    }

    #[tokio::test]
    async fn test_sign_order_action_deterministic_recovery() {
        let signer = HyperliquidSigner::new(TEST_KEY).unwrap();
        let action = json!({
            "type": "order",
            "orders": [{
                "coin": "BTC",
                "is_buy": true,
                "sz": "0.00057",
                "limit_px": "70000.0",
                "order_type": {"limit": {"tif": "Gtc"}},
                "reduce_only": false
            }]
        });
        let nonce = 1729771200000u64;

        let sig = signer.sign_action(&action, nonce).await.unwrap();
        assert!(sig.r.starts_with("0x"));
        assert!(sig.s.starts_with("0x"));
        assert!(sig.v == 27 || sig.v == 28);
    }
}
