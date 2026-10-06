use domain::fsm::FsmError;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error(transparent)]
    Fsm(#[from] FsmError),
    #[error("order {id} changed concurrently (expected version {expected})")]
    Conflict { id: i64, expected: i32 },
    #[error("order {0} not found")]
    NotFound(i64),
    /// Индекс `orders_intake_param_live`: этот чек уже обрабатывается (SPEC §6.3, E16).
    #[error("this check is already being processed in another order")]
    DuplicateLiveCheck,
    /// Индексы `orders_invoice_param_*`: счёт уже в работе или уже оплачен нами.
    #[error("this invoice is already being processed or was already paid")]
    DuplicateInvoice,
    #[error("ledger account {0} does not exist")]
    UnknownAccount(String),
    /// Индекс `ledger_tx_once_per_operation`: повторная обработка ответа не задвоит леджер.
    #[error("operation {0} has already been posted")]
    AlreadyPosted(i64),
    #[error("unexpected value in {column}: {value}")]
    Corrupt { column: &'static str, value: String },
}

impl StorageError {
    pub(crate) fn corrupt(column: &'static str, value: impl ToString) -> Self {
        Self::Corrupt {
            column,
            value: value.to_string(),
        }
    }
}

/// Имя нарушенного ограничения или индекса, если ошибка — нарушение уникальности.
pub(crate) fn unique_violation(err: &sqlx::Error) -> Option<&str> {
    match err {
        sqlx::Error::Database(db) if db.is_unique_violation() => db.constraint(),
        _ => None,
    }
}
