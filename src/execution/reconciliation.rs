//! Idempotent state reconciliation comparing exchange-reported state against local state.

use crate::execution::client::ExchangeClient;
use crate::state::{PositionState, TradingStateMachine};
use crate::types::Quantity;
use thiserror::Error;
use tracing::{info, warn};

#[derive(Error, Debug)]
pub enum ReconciliationError {
    #[error("Failed to query exchange during reconciliation: {0}")]
    QueryFailed(String),
}

/// Detailed outcome of a state reconciliation check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconciliationReport {
    pub is_synced: bool,
    pub exchange_position_size: Quantity,
    pub local_position_size: Quantity,
    pub active_open_orders_count: usize,
    pub discrepancies: Vec<String>,
}

/// Perform idempotent state reconciliation between local TradingStateMachine and the exchange.
pub async fn reconcile_state(
    client: &dyn ExchangeClient,
    account_address: &str,
    local_state: &TradingStateMachine,
    target_coin: &str,
) -> Result<ReconciliationReport, ReconciliationError> {
    info!(
        "Running state reconciliation against exchange for account {}",
        account_address
    );

    let user_state = client
        .get_user_state(account_address)
        .await
        .map_err(|e| ReconciliationError::QueryFailed(e.to_string()))?;

    let open_orders = client
        .get_open_orders(account_address)
        .await
        .map_err(|e| ReconciliationError::QueryFailed(e.to_string()))?;

    let mut discrepancies = Vec::new();

    // 1. Find exchange position for target coin
    let exchange_pos = user_state
        .positions
        .iter()
        .find(|p| p.coin.eq_ignore_ascii_case(target_coin));

    let exchange_size = exchange_pos.map(|p| p.szi).unwrap_or_else(Quantity::zero);

    // 2. Extract local position size
    let local_size = match local_state.position() {
        PositionState::Flat => Quantity::zero(),
        PositionState::Opening { target_size, .. } => *target_size,
        PositionState::OpenProtected { size, .. } => *size,
        PositionState::Closing { closing_size, .. } => *closing_size,
    };

    // 3. Compare position quantities
    if exchange_size != local_size {
        let msg = format!(
            "Position size discrepancy: exchange reports {}, local state expects {}",
            exchange_size, local_size
        );
        warn!("{}", msg);
        discrepancies.push(msg);
    }

    // 4. Check protective orders when in OpenProtected state
    if let PositionState::OpenProtected {
        tp_cloid, sl_cloid, ..
    } = local_state.position()
    {
        let has_tp = open_orders
            .iter()
            .any(|o| o.cloid.as_deref() == Some(tp_cloid.as_str()));
        let has_sl = open_orders
            .iter()
            .any(|o| o.cloid.as_deref() == Some(sl_cloid.as_str()));

        if !has_tp {
            let msg = format!(
                "Missing exchange Take-Profit protective order with cloid {}",
                tp_cloid
            );
            warn!("{}", msg);
            discrepancies.push(msg);
        }
        if !has_sl {
            let msg = format!(
                "Missing exchange Stop-Loss protective order with cloid {}",
                sl_cloid
            );
            warn!("{}", msg);
            discrepancies.push(msg);
        }
    }

    let is_synced = discrepancies.is_empty();

    Ok(ReconciliationReport {
        is_synced,
        exchange_position_size: exchange_size,
        local_position_size: local_size,
        active_open_orders_count: open_orders.len(),
        discrepancies,
    })
}
