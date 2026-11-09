use crate::domain::ReconciliationStatus;
mod domain;
mod error;
mod rpc;
mod store;
use clap::{Parser, Subcommand};
use domain::{Payment, Report};
use error::{Result, invalid};
use futures::{StreamExt, stream};
use serde_json::json;
use std::{path::PathBuf, time::Duration};
#[derive(Parser)]
#[command(
    version,
    about = "Read-only, conservative SPL Token payment reconciliation"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Reconcile(Options),
}
#[derive(Clone, Parser)]
struct Options {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output: PathBuf,
    /// Prefer RPC_URL for endpoints containing credentials; never persisted or logged.
    #[arg(long, env = "RPC_URL", hide_env_values = true)]
    rpc_url: Option<String>,
    #[arg(long,default_value_t=64,value_parser=clap::value_parser!(u16).range(1..=256))]
    batch_size: u16,
    #[arg(long,default_value_t=4,value_parser=clap::value_parser!(u16).range(1..=64))]
    max_concurrency: u16,
    #[arg(long,default_value_t=10000,value_parser=clap::value_parser!(u64).range(1..=120000))]
    timeout_ms: u64,
    #[arg(long,default_value_t=3,value_parser=clap::value_parser!(u32).range(0..=10))]
    max_retries: u32,
    #[arg(long)]
    resume: bool,
}
async fn check_batch(
    rpc: &rpc::Rpc,
    items: Vec<(i64, Payment)>,
    concurrency: usize,
) -> Vec<Report> {
    if items.is_empty() {
        return Vec::new();
    }
    let signatures: Vec<_> = items.iter().map(|(_, p)| p.tx_signature.as_str()).collect();
    let statuses = rpc
        .call(
            "getSignatureStatuses",
            json!([signatures,{"searchTransactionHistory":true}]),
        )
        .await;
    let statuses = match statuses {
        Ok(v) => match v["value"].as_array().filter(|a| a.len() == items.len()) {
            Some(a) => Ok(a.clone()),
            None => Err("rpc_status_shape"),
        },
        Err(e) => Err(e.reason()),
    };
    stream::iter(
        items
            .into_iter()
            .enumerate()
            .map(|(index, (row, payment))| {
                let statuses = &statuses;
                async move {
                    match statuses {
                        Ok(values) => check_payment(rpc, row, &payment, &values[index]).await,
                        Err(reason) => {
                            let mut report = Report::new(row, &payment);
                            report.classify(ReconciliationStatus::NeedsReview, reason);
                            report
                        }
                    }
                }
            }),
    )
    .buffer_unordered(concurrency)
    .collect()
    .await
}

async fn check_payment(
    rpc: &rpc::Rpc,
    row: i64,
    p: &Payment,
    status: &serde_json::Value,
) -> Report {
    use domain::{ChainStatus, ReconciliationStatus::*};
    let mut report = Report::new(row, p);
    if status.is_null() {
        report.classify(
            NeedsReview,
            "signature_not_found_history_may_be_unavailable",
        );
        return report;
    }
    report.slot = status["slot"].as_u64();
    if !status.get("err").is_some_and(serde_json::Value::is_null) {
        report.chain_status = if status["err"].is_object() || status["err"].is_string() {
            ChainStatus::Failed
        } else {
            ChainStatus::Unknown
        };
        report.classify(NeedsReview, "status_error_or_missing");
        report.evidence = json!({"status_err": status["err"]}).to_string();
        return report;
    }
    report.chain_status = ChainStatus::from_confirmation(&status["confirmationStatus"]);
    if report.slot.is_none() {
        report.classify(NeedsReview, "missing_status_slot");
        return report;
    }
    if report.chain_status != ChainStatus::Finalized {
        report.classify(NeedsReview, "not_finalized");
        return report;
    }
    match rpc.call("getTransaction", json!([p.tx_signature, {
        "encoding":"jsonParsed", "commitment":"finalized", "maxSupportedTransactionVersion":0
    }])).await {
        Ok(tx) => {
            if !tx.is_null() && (tx["transaction"]["signatures"][0] != p.tx_signature || tx["slot"].as_u64() != report.slot) {
                report.classify(NeedsReview, "transaction_identity_or_slot_disagreement");
            } else {
                domain::reconcile(p, &tx, &mut report);
            }
        }
        Err(error) => report.classify(
            if error.reason() == "rpc_unsupported_version" { Unsupported } else { NeedsReview },
            error.reason(),
        ),
    }
    report
}

#[tokio::main]
async fn main() {
    let Cli {
        command: Command::Reconcile(options),
    } = Cli::parse();
    let task = tokio::task::spawn_blocking(move || run(options));
    tokio::select! {
     result=task=>match result{Ok(Ok(()))=>{},Ok(Err(e))=>{eprintln!("{e}");std::process::exit(1);},Err(_)=>{eprintln!("worker failed");std::process::exit(1);}},
     _=tokio::signal::ctrl_c()=>{eprintln!("Cancelled; committed batches are safe. Resume with identical options.");std::process::exit(130);}
    }
}
fn run(o: Options) -> Result<()> {
    let endpoint = o
        .rpc_url
        .clone()
        .ok_or_else(|| invalid("Specify --rpc-url or RPC_URL (see offline demo)"))?;
    let url = reqwest::Url::parse(&endpoint).map_err(|_| invalid("Invalid RPC endpoint"))?;
    if url.scheme() != "https"
        && !(url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
    {
        return Err(invalid("RPC requires HTTPS except loopback mock servers"));
    }
    let rpc = rpc::Rpc::new(endpoint, o.max_retries, Duration::from_millis(o.timeout_ms))
        .map_err(|_| invalid("Cannot initialize verified HTTP client"))?;
    let handle = tokio::runtime::Handle::current();
    let mut store = store::Store::open(&o)?;
    let mut input = store::Input::open(&o.output.join("input.snapshot"))?;
    let mut row = 0;
    loop {
        let mut items = Vec::new();
        let mut immediate = Vec::new();
        for _ in 0..o.batch_size {
            let Some(p) = input.next()? else {
                break;
            };
            row += 1;
            if row <= store.progress {
                continue;
            }
            if !p.valid() {
                let mut r = Report::new(row, &p);
                r.classify(
                    ReconciliationStatus::InvalidInput,
                    "invalid_address_signature_or_atomic_amount",
                );
                immediate.push(r);
            } else {
                items.push((row, p));
            }
        }
        if row <= store.progress && items.is_empty() && immediate.is_empty() {
            if input.eof {
                break;
            }
            continue;
        }
        let mut reports = handle.block_on(check_batch(&rpc, items, o.max_concurrency as usize));
        reports.extend(immediate);
        reports.sort_by_key(|r| r.input_row);
        if reports.is_empty() {
            break;
        }
        store.commit(&mut reports, row)?;
        store.export(&o.output)?;
        eprintln!("Committed through input row {row}");
        if input.eof {
            break;
        }
    }
    store.export(&o.output)?;
    store.summary()?;
    Ok(())
}
