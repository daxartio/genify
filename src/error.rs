#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("template rendering failed: {0}")]
    Tera(tera::Error),
    #[error("I/O error: {0}")]
    IOError(std::io::Error),
    #[error("{0}")]
    Operation(String),
}
