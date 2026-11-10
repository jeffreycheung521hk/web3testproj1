use snafu::Snafu;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum Error {
    #[snafu(display("{operation}: {source}"))]
    Io {
        operation: &'static str,
        source: std::io::Error,
    },
    #[snafu(display("CSV input/report failed: {source}"))]
    Csv { source: csv::Error },
    #[snafu(display("Persistence failed: {source}"))]
    Db { source: rusqlite::Error },
    #[snafu(display("JSON processing failed: {source}"))]
    Json { source: serde_json::Error },
    #[snafu(display("Cannot initialize verified HTTP client"))]
    HttpClient { source: reqwest::Error },
    #[snafu(display("RPC infrastructure failure ({}); current batch was not committed; resume after correcting access or retry settings", source.code()))]
    Infrastructure { source: crate::rpc::Failure },
    #[snafu(display("RPC result does not match {operation}; current batch was not committed"))]
    RpcData {
        operation: &'static str,
        source: serde_json::Error,
    },
    #[snafu(display("RPC result invalid: {operation}; current batch was not committed"))]
    RpcShape { operation: &'static str },
    #[snafu(display("Cancelled; committed batches are preserved"))]
    Cancelled,
    #[snafu(display("{primary}; {operation} also failed: {source}"))]
    Finalization {
        operation: &'static str,
        primary: Box<Error>,
        source: Box<Error>,
    },
    #[snafu(display("{message}"))]
    Invalid { message: String },
}
impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::Io {
            operation: "I/O operation failed",
            source,
        }
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
