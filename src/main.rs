use clap::{Parser, Subcommand};
use payment_reconcile::{Options, error::Error, run_with_cancellation};

#[derive(Parser)]
#[command(version, about = "Read-only SPL Token payment reconciliation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Reconcile(Options),
}

#[tokio::main]
async fn main() {
    let Cli {
        command: Command::Reconcile(options),
    } = Cli::parse();
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let mut worker = tokio::task::spawn_blocking(move || run_with_cancellation(options, receiver));
    let result = tokio::select! {
        result = &mut worker => result,
        signal = tokio::signal::ctrl_c() => {
            if signal.is_ok() { let _ = stop.send(true); }
            worker.await
        }
    };
    let code = match result {
        Ok(Ok(())) => 0,
        Ok(Err(error)) => {
            eprintln!("{error}");
            if matches!(error, Error::Cancelled) {
                130
            } else {
                1
            }
        }
        Err(_) => {
            eprintln!("Reconciliation worker failed");
            1
        }
    };
    std::process::exit(code);
}
