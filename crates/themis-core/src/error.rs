//! Error types for Themis core.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("template error: {0}")]
    Template(String),

    #[error("platform error: {0}")]
    Platform(String),

    #[error("parameter validation failed: {0}")]
    InvalidParameter(String),

    #[error("unknown template: {0}")]
    UnknownTemplate(String),

    #[error("unknown platform: {0}")]
    UnknownPlatform(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("runtime error: {0}")]
    Runtime(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("address parse error: {0}")]
    AddrParse(#[from] std::net::AddrParseError),

    #[error("network parse error: {0}")]
    NetParse(#[from] ipnet::AddrParseError),
}
