use thiserror::Error;

#[derive(Error, Debug)]
#[allow(dead_code)]
pub enum LeshyError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("DNS error: {0}")]
    Dns(String),

    #[error("Routing error: {0}")]
    Routing(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parse error: {0}")]
    Parse(String),
}

#[allow(dead_code)]
pub type Result<T> = std::result::Result<T, LeshyError>;
