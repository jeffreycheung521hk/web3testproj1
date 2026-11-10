use crate::{
    Options,
    domain::{self, ChainStatus, Payment, ReconciliationStatus, Report},
    error::{self, Error, Result, invalid},
    model::{ErrorField, SignatureStatus, SignatureStatuses, Transaction},
    reason::Reason,
    rpc::Rpc,
    store::{Input, Store},
};
use futures::{StreamExt, TryStreamExt, stream};
use serde_json::json;
use snafu::ResultExt;
use std::time::Duration;
use tokio::sync::watch;

type InputBatch = Vec<(i64, Payment)>;

/// Run on a blocking worker while a Tokio runtime is active.
pub fn run(options: Options) -> Result<()> {
    let (_sender, receiver) = watch::channel(false);
    run_with_cancellation(options, receiver)
}

pub fn run_with_cancellation(
    options: Options,
    mut cancellation: watch::Receiver<bool>,
) -> Result<()> {
    options.validate()?;
    let mut store = Store::open(&options)?;
    let processing = if options.export_only {
        Ok(())
    } else {
        process(&options, &mut store, &mut cancellation)
    };
    // One CSV export per invocation, including handled failure/cancellation.
    let export = store.export(&options.output);
    let report_available = export.is_ok();
    let processing_complete = processing.is_ok() && !options.export_only;
    let state = if !report_available {
        "export_error"
    } else if options.export_only {
        "exported"
    } else {
        match &processing {
            Ok(()) => "complete",
            Err(Error::Cancelled) => "cancelled",
            Err(Error::Infrastructure { .. } | Error::RpcData { .. } | Error::RpcShape { .. }) => {
                "infrastructure_error"
            }
            Err(_) => "error",
        }
    };
    let code = if !report_available {
        Some("report_export_failed")
    } else {
        match &processing {
            Err(Error::Infrastructure { source }) => Some(source.code()),
            Err(Error::RpcData { .. } | Error::RpcShape { .. }) => Some("rpc_invalid_result"),
            Err(Error::Cancelled) => Some("cancelled"),
            Err(_) => Some("processing_error"),
            Ok(()) => None,
        }
    };
    let manifest = store.write_run_manifest(
        &options.output,
        state,
        code,
        report_available,
        processing_complete,
    );
    let outcome = match (processing, export) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(source)) => Err(Error::Finalization {
            operation: "exporting committed results",
            primary: Box::new(primary),
            source: Box::new(source),
        }),
    };
    match (outcome, manifest) {
        (Ok(()), Ok(())) => store.summary(),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(source)) => Err(Error::Finalization {
            operation: "writing run summary",
            primary: Box::new(primary),
            source: Box::new(source),
        }),
    }
}

fn process(
    options: &Options,
    store: &mut Store,
    cancellation: &mut watch::Receiver<bool>,
) -> Result<()> {
    let endpoint = options
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
    let rpc = Rpc::with_limits(
        endpoint,
        options.max_retries,
        Duration::from_millis(options.timeout_ms),
        options.max_rps,
        options.circuit_breaker_threshold,
    )
    .map_err(reqwest::Error::without_url)
    .context(error::HttpClientSnafu)?;
    let handle = tokio::runtime::Handle::current();
    let genesis = handle.block_on(cancellable(cancellation, async {
        let value = rpc
            .call("getGenesisHash", json!([]))
            .await
            .context(error::InfrastructureSnafu)?;
        let hash = value
            .as_str()
            .filter(|value| {
                bs58::decode(value)
                    .into_vec()
                    .is_ok_and(|bytes| bytes.len() == 32)
            })
            .ok_or(Error::RpcShape {
                operation: "getGenesisHash",
            })?;
        Ok(hash.to_owned())
    }))?;
    store.bind_network(&genesis)?;
    let review_limit = store.progress;
    if options.retry_review {
        let mut cursor = 0;
        loop {
            let batch =
                store.review_batch(cursor, review_limit, usize::from(options.batch_size))?;
            if batch.is_empty() {
                break;
            }
            cursor = batch.last().map_or(cursor, |(row, _)| *row);
            let mut reports = handle.block_on(cancellable(
                cancellation,
                evaluate(&rpc, &batch, usize::from(options.max_concurrency)),
            ))?;
            store.revise(&mut reports)?;
            eprintln!("Rechecked committed rows through {cursor}");
        }
    }
    let mut input = Input::open(&options.output.join("input.snapshot"))?;
    let mut row = 0i64;
    loop {
        if *cancellation.borrow() {
            return Err(Error::Cancelled);
        }
        let mut batch: InputBatch = Vec::new();
        for _ in 0..options.batch_size {
            let Some(payment) = input.read_payment()? else {
                break;
            };
            row = row
                .checked_add(1)
                .ok_or_else(|| invalid("Input row index exceeded supported range"))?;
            if row > store.progress {
                batch.push((row, payment));
            }
        }
        if batch.is_empty() {
            if input.eof {
                break;
            }
            continue;
        }
        let mut reports = handle.block_on(cancellable(
            cancellation,
            evaluate(&rpc, &batch, usize::from(options.max_concurrency)),
        ))?;
        if *cancellation.borrow() {
            return Err(Error::Cancelled);
        }
        store.commit(&mut reports, &batch, row)?;
        eprintln!("Committed through input row {row}");
        if input.eof {
            break;
        }
    }
    Ok(())
}

async fn cancellable<T>(
    receiver: &mut watch::Receiver<bool>,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    if *receiver.borrow() {
        return Err(Error::Cancelled);
    }
    tokio::select! {
        result = future => result,
        _ = receiver.changed() => Err(Error::Cancelled),
    }
}

async fn evaluate(rpc: &Rpc, batch: &[(i64, Payment)], concurrency: usize) -> Result<Vec<Report>> {
    let mut invalid = Vec::new();
    let mut valid = Vec::new();
    for (row, payment) in batch {
        if let Err(reason) = payment.validate() {
            let mut report = Report::new(*row, payment);
            report.classify(ReconciliationStatus::InvalidInput, reason);
            invalid.push(report);
        } else {
            valid.push((*row, payment));
        }
    }
    if !valid.is_empty() {
        let signatures: Vec<_> = valid.iter().map(|(_, p)| &p.tx_signature).collect();
        let value = rpc
            .call(
                "getSignatureStatuses",
                json!([signatures, {"searchTransactionHistory": true}]),
            )
            .await
            .context(error::InfrastructureSnafu)?;
        let statuses: SignatureStatuses =
            serde_json::from_value(value).context(error::RpcDataSnafu {
                operation: "getSignatureStatuses",
            })?;
        if statuses.value.len() != valid.len() {
            return Err(Error::RpcShape {
                operation: "getSignatureStatuses value count",
            });
        }
        let checked: Vec<Report> = stream::iter(valid.into_iter().zip(statuses.value).map(
            |((row, payment), status)| async move {
                check_payment(rpc, row, payment, status.as_ref()).await
            },
        ))
        .buffer_unordered(concurrency)
        .try_collect()
        .await?;
        invalid.extend(checked);
    }
    invalid.sort_by_key(|r| r.input_row);
    Ok(invalid)
}

async fn check_payment(
    rpc: &Rpc,
    row: i64,
    payment: &Payment,
    status: Option<&SignatureStatus>,
) -> Result<Report> {
    let mut report = Report::new(row, payment);
    let Some(status) = status else {
        report.classify(ReconciliationStatus::NeedsReview, Reason::SignatureNotFound);
        return Ok(report);
    };
    report.slot = status.slot;
    if matches!(status.err, ErrorField::Missing | ErrorField::Invalid) {
        return Err(Error::RpcShape {
            operation: "signature execution status",
        });
    }
    if status
        .confirmation_status
        .as_deref()
        .is_some_and(|value| !matches!(value, "processed" | "confirmed" | "finalized"))
    {
        return Err(Error::RpcShape {
            operation: "signature confirmation status",
        });
    }
    report.chain_status = status.chain_status();
    if let Some(failure) = status.execution_error() {
        report.classify(
            ReconciliationStatus::NeedsReview,
            Reason::StatusErrorOrMissing,
        );
        report.evidence = json!({"status_err": failure}).to_string();
        return Ok(report);
    }
    if report.slot.is_none() {
        return Err(Error::RpcShape {
            operation: "signature slot",
        });
    }
    if report.chain_status != ChainStatus::Finalized {
        report.classify(ReconciliationStatus::NeedsReview, Reason::NotFinalized);
        return Ok(report);
    }
    let value = match rpc.call("getTransaction", json!([payment.tx_signature, {"encoding":"jsonParsed", "commitment":"finalized", "maxSupportedTransactionVersion":0}])).await {
        Ok(value) => value,
        Err(error) if error.is_unsupported_version() => {
            report.classify(ReconciliationStatus::Unsupported, Reason::TransactionVersion);
            return Ok(report);
        }
        Err(source) => return Err(Error::Infrastructure { source }),
    };
    if value.is_null() {
        report.classify(
            ReconciliationStatus::NeedsReview,
            Reason::TransactionUnavailable,
        );
        return Ok(report);
    }
    let transaction = Transaction::parse(value).context(error::RpcDataSnafu {
        operation: "getTransaction",
    })?;
    if transaction.first_signature() != Some(payment.tx_signature.as_str())
        || transaction.slot != report.slot
    {
        return Err(Error::RpcShape {
            operation: "transaction identity or slot",
        });
    }
    if transaction
        .meta
        .as_ref()
        .is_some_and(|meta| matches!(meta.err, ErrorField::Error(_) | ErrorField::Invalid))
    {
        return Err(Error::RpcShape {
            operation: "successful signature status contradicts transaction execution status",
        });
    }
    domain::reconcile_transaction(payment, &transaction, &mut report);
    Ok(report)
}
