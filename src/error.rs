use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("HTTP error: {0}")]
    Http(String),

    #[error("Invalid embedding: {0}")]
    InvalidEmbedding(String),

    #[error("Import script failed: {0}")]
    ImportFailed(String),

    #[error("Invalid date format: {0}")]
    InvalidDate(String),

    #[error("No import script found for {bank}/{account}")]
    NoImportScript { bank: String, account: String },

    #[error("Category already exists: {0}")]
    CategoryExists(String),
}

pub type Result<T> = std::result::Result<T, Error>;
