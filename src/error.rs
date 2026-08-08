#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("template rendering failed: {0}")]
    Tera(#[from] tera::Error),
    #[error("I/O error: {0}")]
    IOError(#[from] std::io::Error),
    #[error("{0}")]
    Operation(String),
}
