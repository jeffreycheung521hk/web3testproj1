use snafu::Snafu;
#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("I/O operation failed: {source}"))]
    Io { source: std::io::Error },
    #[snafu(display("CSV input/report failed: {source}"))]
    Csv { source: csv::Error },
    #[snafu(display("Persistence failed: {source}"))]
    Db { source: rusqlite::Error },
    #[snafu(display("JSON processing failed: {source}"))]
    Json { source: serde_json::Error },
    #[snafu(display("{message}"))]
    Invalid { message: String },
}
impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::Io { source }
    }
}
impl From<csv::Error> for Error {
    fn from(source: csv::Error) -> Self {
        Self::Csv { source }
    }
}
impl From<rusqlite::Error> for Error {
    fn from(source: rusqlite::Error) -> Self {
        Self::Db { source }
    }
}
impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Self::Json { source }
    }
}
pub type Result<T> = std::result::Result<T, Error>;
pub fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid {
        message: message.into(),
    }
}
