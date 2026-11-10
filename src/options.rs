use clap::Parser;
use std::path::PathBuf;

#[derive(Clone, Parser)]
pub struct Options {
    #[arg(long)]
    pub input: PathBuf,
    #[arg(long)]
    pub output: PathBuf,
    /// Prefer RPC_URL for endpoints containing credentials.
    #[arg(long, env = "RPC_URL", hide_env_values = true)]
    pub rpc_url: Option<String>,
    #[arg(long, default_value_t = 64, value_parser = clap::value_parser!(u16).range(1..=256))]
    pub batch_size: u16,
    #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u16).range(1..=64))]
    pub max_concurrency: u16,
    #[arg(long, default_value_t = 10000, value_parser = clap::value_parser!(u64).range(1..=120000))]
    pub timeout_ms: u64,
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u32).range(0..=10))]
    pub max_retries: u32,
    /// Maximum request starts per second, including retries and network identity checks.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u16).range(1..=1000))]
    pub max_rps: u16,
    #[arg(long, default_value_t = 3, value_parser = clap::value_parser!(u16).range(1..=100))]
    pub circuit_breaker_threshold: u16,
    #[arg(long)]
    pub resume: bool,
    /// Rebuild committed CSV results without network access.
    #[arg(long, requires = "resume", conflicts_with = "retry_review")]
    pub export_only: bool,
    /// Recheck previously committed needs_review rows, preserving previous reports in SQLite.
    #[arg(long, requires = "resume")]
    pub retry_review: bool,
}

impl Options {
    /// Validate library callers as well as command-line bounds before touching output files.
    pub fn validate(&self) -> crate::error::Result<()> {
        if !(1..=256).contains(&self.batch_size)
            || !(1..=64).contains(&self.max_concurrency)
            || !(1..=120_000).contains(&self.timeout_ms)
            || self.max_retries > 10
            || !(1..=1_000).contains(&self.max_rps)
            || !(1..=100).contains(&self.circuit_breaker_threshold)
        {
            return Err(crate::error::invalid(
                "Execution limits exceed supported ranges",
            ));
        }
        if (self.export_only || self.retry_review) && !self.resume {
            return Err(crate::error::invalid(
                "Export-only and retry-review require resume",
            ));
        }
        if self.export_only && self.retry_review {
            return Err(crate::error::invalid(
                "Export-only cannot recheck review rows",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn library_options_validate_bounds_and_dependencies() {
        let mut options =
            Options::try_parse_from(["test", "--input", "input.csv", "--output", "out"])
                .expect("valid options");
        assert!(options.validate().is_ok());
        options.batch_size = 0;
        assert!(options.validate().is_err());
        options.batch_size = 1;
        options.max_concurrency = 0;
        assert!(options.validate().is_err());
        options.max_concurrency = 1;
        options.export_only = true;
        assert!(options.validate().is_err());
        options.resume = true;
        assert!(options.validate().is_ok());
        options.retry_review = true;
        assert!(options.validate().is_err());
    }
}
