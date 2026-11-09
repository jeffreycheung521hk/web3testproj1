use serde::{Deserialize, Serialize};
use serde_json::Value;
pub const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Payment {
    pub payment_id: String,
    pub tx_signature: String,
    pub expected_sender_owner: String,
    pub expected_recipient_owner: String,
    pub expected_mint: String,
    pub expected_amount_atomic: String,
}
pub fn atomic(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0'))
    {
        return None;
    }
    s.parse().ok()
}
fn address(s: &str, n: usize) -> bool {
    bs58::decode(s).into_vec().is_ok_and(|v| v.len() == n)
}
impl Payment {
    pub fn valid(&self) -> bool {
        !self.payment_id.is_empty()
            && self.payment_id.len() <= 256
            && address(&self.tx_signature, 64)
            && [
                &self.expected_sender_owner,
                &self.expected_recipient_owner,
                &self.expected_mint,
            ]
            .iter()
            .all(|s| address(s, 32))
            && atomic(&self.expected_amount_atomic).is_some_and(|n| n > 0)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChainStatus {
    Unknown,
    Processed,
    Confirmed,
    Finalized,
    Failed,
}
impl ChainStatus {
    pub fn from_confirmation(value: &Value) -> Self {
        match value.as_str() {
            Some("processed") => Self::Processed,
            Some("confirmed") => Self::Confirmed,
            Some("finalized") => Self::Finalized,
            _ => Self::Unknown,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationStatus {
    Matched,
    Mismatch,
    NeedsReview,
    Unsupported,
    InvalidInput,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub input_row: i64,
    pub payment_id: String,
    pub signature: String,
    pub chain_status: ChainStatus,
    pub reconciliation_status: ReconciliationStatus,
    pub reason: String,
    pub slot: Option<u64>,
    pub checked_at_unix: u64,
    pub fee_lamports: Option<u64>,
    pub evidence: String,
}
impl Report {
    pub fn new(row: i64, p: &Payment) -> Self {
        Self {
            input_row: row,
            payment_id: p.payment_id.clone(),
            signature: p.tx_signature.clone(),
            chain_status: ChainStatus::Unknown,
            reconciliation_status: ReconciliationStatus::NeedsReview,
            reason: String::new(),
            slot: None,
            checked_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            fee_lamports: None,
            evidence: String::new(),
        }
    }
    pub fn classify(&mut self, status: ReconciliationStatus, reason: &str) {
        self.reconciliation_status = status;
        self.reason = reason.into();
    }
}
// Deliberately narrow evidence gate: exactly one outer token transfer, no CPI.
pub fn reconcile(p: &Payment, tx: &Value, r: &mut Report) {
    if tx.is_null() {
        r.classify(ReconciliationStatus::NeedsReview, "transaction_unavailable");
        return;
    }
    r.slot = tx["slot"].as_u64();
    r.fee_lamports = tx["meta"]["fee"].as_u64();
    if tx["version"] != "legacy" && tx["version"] != 0 {
        r.classify(ReconciliationStatus::Unsupported, "transaction_version");
        return;
    }
    let Some(meta) = tx.get("meta").filter(|v| v.is_object()) else {
        r.classify(ReconciliationStatus::NeedsReview, "missing_meta");
        return;
    };
    if !meta.get("err").is_some_and(Value::is_null) {
        if meta["err"].is_object() || meta["err"].is_string() {
            r.chain_status = ChainStatus::Failed;
        }
        r.classify(
            ReconciliationStatus::NeedsReview,
            "transaction_execution_error_or_missing",
        );
        return;
    }
    let Some(ixs) = tx["transaction"]["message"]["instructions"].as_array() else {
        r.classify(ReconciliationStatus::Unsupported, "missing_instructions");
        return;
    };
    if ixs.len() != 1
        || !meta["innerInstructions"]
            .as_array()
            .is_some_and(Vec::is_empty)
    {
        r.classify(
            ReconciliationStatus::Unsupported,
            "complex_or_missing_inner_instructions",
        );
        return;
    }
    let ix = &ixs[0];
    let info = &ix["parsed"]["info"];
    if ix["programId"] != TOKEN
        || !["transfer", "transferChecked"]
            .iter()
            .any(|t| ix["parsed"]["type"] == *t)
    {
        r.classify(
            ReconciliationStatus::Unsupported,
            "non_standard_token_transfer",
        );
        return;
    }
    let keys = &tx["transaction"]["message"]["accountKeys"];
    let Some(keys) = keys.as_array() else {
        r.classify(ReconciliationStatus::NeedsReview, "missing_accounts");
        return;
    };
    if !keys
        .iter()
        .any(|k| k["pubkey"] == TOKEN && k["signer"] == false)
    {
        r.classify(
            ReconciliationStatus::NeedsReview,
            "token_program_account_missing",
        );
        return;
    }
    let Some(src) = keys.iter().position(|k| k["pubkey"] == info["source"]) else {
        r.classify(ReconciliationStatus::NeedsReview, "source_account_missing");
        return;
    };
    let Some(dst) = keys.iter().position(|k| k["pubkey"] == info["destination"]) else {
        r.classify(
            ReconciliationStatus::NeedsReview,
            "destination_account_missing",
        );
        return;
    };
    if src == dst {
        r.classify(ReconciliationStatus::Unsupported, "self_transfer");
        return;
    }
    let find = |field: &str, index: usize| -> Option<&Value> {
        let mut matching = meta[field]
            .as_array()?
            .iter()
            .filter(|v| v["accountIndex"].as_u64() == Some(index as u64));
        let first = matching.next()?;
        if matching.next().is_some() {
            None
        } else {
            Some(first)
        }
    };
    let (Some(a), Some(b), Some(c), Some(d)) = (
        find("preTokenBalances", src),
        find("postTokenBalances", src),
        find("preTokenBalances", dst),
        find("postTokenBalances", dst),
    ) else {
        r.classify(ReconciliationStatus::NeedsReview, "missing_token_balances");
        return;
    };
    if [a, b, c, d].iter().any(|v| {
        v["programId"] != TOKEN || v["owner"].as_str().is_none() || v["mint"].as_str().is_none()
    }) {
        r.classify(
            ReconciliationStatus::NeedsReview,
            "missing_owner_mint_or_token_program",
        );
        return;
    }
    if a["owner"] != b["owner"]
        || c["owner"] != d["owner"]
        || [b, c, d].iter().any(|v| v["mint"] != a["mint"])
    {
        r.classify(
            ReconciliationStatus::Unsupported,
            "inconsistent_account_metadata",
        );
        return;
    }
    let amount = info["amount"]
        .as_str()
        .or_else(|| info["tokenAmount"]["amount"].as_str())
        .and_then(atomic);
    let bal = |v: &Value| v["uiTokenAmount"]["amount"].as_str().and_then(atomic);
    let evidence = (|| {
        Some((
            amount?,
            bal(a)?.checked_sub(bal(b)?)?,
            bal(d)?.checked_sub(bal(c)?)?,
        ))
    })();
    let Some((amount, debit, credit)) = evidence else {
        r.classify(ReconciliationStatus::NeedsReview, "invalid_atomic_evidence");
        return;
    };
    if amount != debit
        || amount != credit
        || (ix["parsed"]["type"] == "transferChecked" && info["mint"] != a["mint"])
    {
        r.classify(
            ReconciliationStatus::NeedsReview,
            "instruction_balance_disagreement",
        );
        return;
    }
    // No multisig/delegate inference: authority must be the source wallet signer.
    if info["authority"] != a["owner"]
        || !keys
            .iter()
            .any(|k| k["pubkey"] == a["owner"] && k["signer"] == true)
    {
        r.classify(
            ReconciliationStatus::Unsupported,
            "authority_not_owner_signer",
        );
        return;
    }
    r.evidence=serde_json::json!({"source":info["source"],"destination":info["destination"],"sender_owner":a["owner"],"recipient_owner":c["owner"],"mint":a["mint"],"amount_atomic":amount,"program":TOKEN}).to_string();
    let mut differences = Vec::new();
    if a["owner"] != p.expected_sender_owner {
        differences.push("sender_owner");
    }
    if c["owner"] != p.expected_recipient_owner {
        differences.push("recipient_owner");
    }
    if a["mint"] != p.expected_mint {
        differences.push("mint");
    }
    if Some(amount) != atomic(&p.expected_amount_atomic) {
        differences.push("amount_atomic");
    }
    if differences.is_empty() {
        r.classify(
            ReconciliationStatus::Matched,
            "finalized_direct_transfer_verified",
        );
    } else {
        r.classify(ReconciliationStatus::Mismatch, &differences.join(";"));
    }
}
