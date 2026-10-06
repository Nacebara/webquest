//! Таблица разрешённых переходов в БД — страховка FSM на уровне триггера.

use domain::fsm::OrderState;
use sqlx::PgConnection;

use crate::error::StorageError;

/// Все строки `order_state_transitions`, отсортированные.
pub async fn load_allowed(
    conn: &mut PgConnection,
) -> Result<Vec<(OrderState, OrderState)>, StorageError> {
    let rows = sqlx::query!(
        r#"SELECT from_state, to_state FROM order_state_transitions ORDER BY from_state, to_state"#
    )
    .fetch_all(conn)
    .await?;
    rows.into_iter()
        .map(|r| {
            let from = r
                .from_state
                .parse()
                .map_err(|_| StorageError::corrupt("from_state", &r.from_state))?;
            let to = r
                .to_state
                .parse()
                .map_err(|_| StorageError::corrupt("to_state", &r.to_state))?;
            Ok((from, to))
        })
        .collect()
}
