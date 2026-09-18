//! Error type shared across the workspace.

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("clipboard: {0}")]
    Clipboard(String),

    #[error("storage: {0}")]
    Storage(String),

    #[error("network: {0}")]
    Network(String),

    #[error("peer unreachable: {peer}")]
    PeerUnreachable { peer: String },

    #[error("entry not found: {0}")]
    NotFound(String),

    #[error("payload too large: {size} bytes (limit {limit})")]
    TooLarge { size: u64, limit: u64 },

    #[error("unauthorized: missing or invalid auth token")]
    Unauthorized,

    #[error("tailscale unavailable: {0}")]
    Tailscale(String),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<redb::DatabaseError> for Error {
    fn from(e: redb::DatabaseError) -> Self {
        Error::Storage(e.to_string())
    }
}

impl From<redb::TransactionError> for Error {
    fn from(e: redb::TransactionError) -> Self {
        Error::Storage(e.to_string())
    }
}

impl From<redb::CommitError> for Error {
    fn from(e: redb::CommitError) -> Self {
        Error::Storage(e.to_string())
    }
}

impl From<redb::TableError> for Error {
    fn from(e: redb::TableError) -> Self {
        Error::Storage(e.to_string())
    }
}

impl From<redb::StorageError> for Error {
    fn from(e: redb::StorageError) -> Self {
        Error::Storage(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Storage(format!("serde: {e}"))
    }
}

impl From<image::ImageError> for Error {
    fn from(e: image::ImageError) -> Self {
        Error::Storage(format!("image: {e}"))
    }
}

impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Error::Network(e.to_string())
    }
}

impl From<axum::Error> for Error {
    fn from(e: axum::Error) -> Self {
        Error::Network(e.to_string())
    }
}
