//! Offline regression coverage from unmodified, provenance-pinned upstream mainnet captures.
use payment_reconcile::domain::{ChainStatus, Payment, ReconciliationStatus, Report, reconcile};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

const WRAP_SOL: &str = include_str!("fixtures/real/sync_native.json");
const SWAP: &str = include_str!("fixtures/real/swap_v0.json");
const FAILED_SWAP: &str = include_str!("fixtures/real/failed_v0.json");

fn check_capture(raw: &str) -> (Value, Report) {
    let capture: Value =
        serde_json::from_str(raw).expect("retained upstream fixture is valid JSON");
    let transaction = capture.get("result").unwrap_or(&capture);
    let payment = Payment {
        payment_id: "synthetic-expectation-for-rejection-test".into(),
        tx_signature: transaction["transaction"]["signatures"][0]
            .as_str()
            .expect("public capture contains its signature")
            .into(),
        expected_sender_owner: "11111111111111111111111111111111".into(),
        expected_recipient_owner: "11111111111111111111111111111111".into(),
        expected_mint: "So11111111111111111111111111111111111111112".into(),
        expected_amount_atomic: "1".into(),
    };
    let mut report = Report::new(1, &payment);
    // Fixture has transaction details only. No captured status/finalization is invented here.
    reconcile(&payment, transaction, &mut report);
    assert_eq!(report.slot, transaction["slot"].as_u64());
    assert_eq!(report.block_time, transaction["blockTime"].as_i64());
    assert_ne!(report.reconciliation_status, ReconciliationStatus::Matched);
    (capture, report)
}

#[test]
fn captured_sol_wrap_is_not_a_direct_spl_payment() {
    let (_, report) = check_capture(WRAP_SOL);
    assert_eq!(
        report.reconciliation_status,
        ReconciliationStatus::Unsupported
    );
    assert_eq!(report.slot, Some(175_687_414));
    assert_eq!(report.block_time, Some(1_675_358_593));
    assert_eq!(report.fee_lamports, Some(5_000));
}

#[test]
fn captured_v0_swap_with_real_compute_budget_and_cpi_stays_unsupported() {
    let (_, report) = check_capture(SWAP);
    assert_eq!(
        report.reconciliation_status,
        ReconciliationStatus::Unsupported
    );
    assert_eq!(report.slot, Some(467_421_519));
    assert_eq!(report.block_time, Some(1_794_249_619));
}

#[test]
fn captured_failed_v0_swap_keeps_execution_failure_separate() {
    let (_, report) = check_capture(FAILED_SWAP);
    assert_eq!(report.chain_status, ChainStatus::Failed);
    assert_eq!(
        report.reconciliation_status,
        ReconciliationStatus::NeedsReview
    );
    assert_eq!(report.slot, Some(467_421_524));
    assert_eq!(report.block_time, Some(1_794_249_621));
}

#[derive(Deserialize)]
struct Provenance {
    independently_requeried: bool,
    records: Vec<Record>,
}
#[derive(Deserialize)]
struct Record {
    file: String,
    original_sha256: String,
    retained_sha256: String,
    modified: bool,
    signature: String,
    slot: u64,
    block_time: i64,
}

#[test]
fn retained_capture_bytes_match_recorded_upstream_checksums_and_identity() {
    let provenance: Provenance =
        serde_json::from_str(include_str!("fixtures/real/provenance.json"))
            .expect("provenance manifest parses");
    assert!(!provenance.independently_requeried);
    assert_eq!(provenance.records.len(), 3);
    for record in provenance.records {
        let raw = match record.file.as_str() {
            "sync_native.json" => WRAP_SOL,
            "swap_v0.json" => SWAP,
            "failed_v0.json" => FAILED_SWAP,
            other => panic!("unrecognized fixture in provenance: {other}"),
        };
        let digest: String = Sha256::digest(raw.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(digest, record.original_sha256);
        assert_eq!(digest, record.retained_sha256);
        assert!(!record.modified);
        let capture: Value = serde_json::from_str(raw).expect("capture JSON parses");
        assert_eq!(capture["transaction"]["signatures"][0], record.signature);
        assert_eq!(capture["slot"].as_u64(), Some(record.slot));
        assert_eq!(capture["blockTime"].as_i64(), Some(record.block_time));
    }
}
