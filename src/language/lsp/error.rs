//! Language-server transport and protocol failures.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum LspError {
    #[error("tool `{0}` is not a language server")]
    NotLanguageServer(String),
    #[error("language server `{0}` is already running")]
    AlreadyRunning(String),
    #[error("language server `{0}` is not running")]
    NotRunning(String),
    #[error("language server `{0}` is busy")]
    Busy(String),
    #[error("language server executable `{0}` is not installed")]
    ExecutableMissing(String),
    #[error("language server launch requires an absolute executable from ExecutionPreflightWorker")]
    PreflightRequired,
    #[error("failed to create language-server thread: {0}")]
    Thread(#[source] std::io::Error),
}
