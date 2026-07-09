use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Failed to parse config {path}: {source}")]
    ConfigParse {
        path: std::path::PathBuf,
        source: toml::de::Error,
    },

    #[error("Invalid config value in {path}: {message}")]
    ConfigValue {
        path: std::path::PathBuf,
        message: String,
    },

    #[error("Import script failed: {0}")]
    ImportFailed(String),

    #[error("Invalid date format: {0}")]
    InvalidDate(String),

    #[error("No import script found for {bank}/{account}")]
    NoImportScript { bank: String, account: String },

    #[error("Category already exists: {0}")]
    CategoryExists(String),

    #[error("Account not found: {0}")]
    AccountNotFound(String),

    #[error("Account already exists: {0}")]
    AccountExists(String),

    #[error("Invalid account path: {0}")]
    InvalidAccountPath(String),

    #[error("Transaction {0} is part of a transfer; unlink the transfer before categorising")]
    TransactionInTransfer(i64),
}

pub type Result<T> = std::result::Result<T, Error>;
