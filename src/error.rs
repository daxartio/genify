#[derive(Debug)]
pub enum Error {
    Tera(tera::Error),
    IOError(std::io::Error),
}
