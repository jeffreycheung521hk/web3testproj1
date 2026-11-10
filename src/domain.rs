use crate::model::{
    AccountKey, ErrorField, Instruction, ParsedInstruction, TokenBalance, Transaction,
};
pub use crate::reason::Reason;
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const COMPUTE_BUDGET: &str = "ComputeBudget111111111111111111111111111111";
pub const MEMO: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";
pub const MEMO_LEGACY: &str = "Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo";
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
    pub fn validate(&self) -> Result<(), Reason> {
        if self.payment_id.is_empty() || self.payment_id.len() > 256 {
            return Err(Reason::InvalidPaymentId);
        }
        if !address(&self.tx_signature, 64) {
            return Err(Reason::InvalidSignature);
        }
        for (value, reason) in [
            (&self.expected_sender_owner, Reason::InvalidSenderOwner),
            (
                &self.expected_recipient_owner,
                Reason::InvalidRecipientOwner,
            ),
            (&self.expected_mint, Reason::InvalidMint),
        ] {
            if !address(value, 32) {
                return Err(reason);
            }
        }
        if !atomic(&self.expected_amount_atomic).is_some_and(|amount| amount > 0) {
            return Err(Reason::InvalidAmount);
        }
        Ok(())
    }
    pub fn valid(&self) -> bool {
        self.validate().is_ok()
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
    pub fn from_name(value: Option<&str>) -> Self {
        match value {
            Some("processed") => Self::Processed,
            Some("confirmed") => Self::Confirmed,
            Some("finalized") => Self::Finalized,
            _ => Self::Unknown,
        }
    }
    pub fn from_confirmation(value: &Value) -> Self {
        Self::from_name(value.as_str())
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
    pub reason: Reason,
    #[serde(default)]
    pub reason_details: String,
    pub slot: Option<u64>,
    #[serde(default)]
    pub block_time: Option<i64>,
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
            reason: Reason::Unclassified,
            reason_details: String::new(),
            slot: None,
            block_time: None,
            checked_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            fee_lamports: None,
            evidence: String::new(),
        }
    }
    pub fn classify(&mut self, status: ReconciliationStatus, reason: Reason) {
        self.reconciliation_status = status;
        self.reason = reason;
    }
}
/// Parse a bounded RPC value and apply the same typed evidence gate used by the CLI.
pub fn reconcile(payment: &Payment, value: &Value, report: &mut Report) {
    if value.is_null() {
        report.classify(
            ReconciliationStatus::NeedsReview,
            Reason::TransactionUnavailable,
        );
        return;
    }
    match serde_json::from_value::<Transaction>(value.clone()) {
        Ok(transaction) => reconcile_transaction(payment, &transaction, report),
        Err(_) => report.classify(
            ReconciliationStatus::NeedsReview,
            Reason::MalformedTransaction,
        ),
    }
}
/// Recognize one direct token transfer, optionally surrounded by known inert/resource instructions.
pub fn reconcile_transaction(p: &Payment, tx: &Transaction, r: &mut Report) {
    use Reason::*;
    use ReconciliationStatus::*;
    let expected_slot = r.slot;
    // Retain available chain metadata even when the evidence gate rejects the transfer.
    r.slot = tx.slot;
    r.block_time = tx.block_time;
    r.fee_lamports = tx.meta.as_ref().and_then(|meta| meta.fee);
    if let Err(reason) = p.validate() {
        r.classify(ReconciliationStatus::InvalidInput, reason);
        return;
    }
    if tx.slot.is_none()
        || tx.first_signature() != Some(p.tx_signature.as_str())
        || expected_slot.is_some_and(|slot| Some(slot) != tx.slot)
    {
        r.classify(NeedsReview, TransactionIdentityOrSlotDisagreement);
        return;
    }
    if !tx.version.as_ref().is_some_and(|v| v.supported()) {
        r.classify(Unsupported, TransactionVersion);
        return;
    }
    let Some(meta) = tx.meta.as_ref() else {
        r.classify(NeedsReview, MissingMeta);
        return;
    };
    r.fee_lamports = meta.fee;
    if meta.err != ErrorField::Null {
        if matches!(meta.err, ErrorField::Error(_)) {
            r.chain_status = ChainStatus::Failed;
        }
        r.classify(NeedsReview, TransactionExecutionErrorOrMissing);
        return;
    }
    let Some(message) = tx.transaction.as_ref().and_then(|t| t.message.as_ref()) else {
        r.classify(NeedsReview, MissingAccounts);
        return;
    };
    let Some(ixs) = message.instructions.as_ref() else {
        r.classify(Unsupported, MissingInstructions);
        return;
    };
    if !meta.inner_instructions.as_ref().is_some_and(Vec::is_empty) {
        r.classify(Unsupported, ComplexOrMissingInnerInstructions);
        return;
    }
    let Some(keys) = message.account_keys.as_ref() else {
        r.classify(NeedsReview, MissingAccounts);
        return;
    };
    let mut transfer = None;
    let mut budget_mask = 0u8;
    for ix in ixs {
        match ix.program_id.as_deref() {
            Some(TOKEN) => {
                if transfer.replace(ix).is_some() {
                    r.classify(Unsupported, ComplexOrMissingInnerInstructions);
                    return;
                }
            }
            Some(COMPUTE_BUDGET | MEMO | MEMO_LEGACY) => {
                if !ancillary_valid(ix, keys, &mut budget_mask) {
                    r.classify(Unsupported, UnsupportedAncillaryInstruction);
                    return;
                }
            }
            _ => {
                r.classify(Unsupported, NonStandardTokenTransfer);
                return;
            }
        }
    }
    let Some(ix) = transfer else {
        r.classify(Unsupported, NonStandardTokenTransfer);
        return;
    };
    let Some(ParsedInstruction::Token(parsed)) = ix.parsed.as_ref() else {
        r.classify(Unsupported, NonStandardTokenTransfer);
        return;
    };
    if !matches!(parsed.kind.as_str(), "transfer" | "transferChecked") {
        r.classify(Unsupported, NonStandardTokenTransfer);
        return;
    }
    let Some(info) = parsed.info.as_ref() else {
        r.classify(NeedsReview, InvalidAtomicEvidence);
        return;
    };
    if !keys
        .iter()
        .any(|key| key.pubkey == TOKEN && key.signer == Some(false))
    {
        r.classify(NeedsReview, TokenProgramAccountMissing);
        return;
    }
    let source = info.source.as_deref();
    let destination = info.destination.as_deref();
    let Some(src) = unique_account(keys, source) else {
        r.classify(NeedsReview, SourceAccountMissing);
        return;
    };
    let Some(dst) = unique_account(keys, destination) else {
        r.classify(NeedsReview, DestinationAccountMissing);
        return;
    };
    if src == dst {
        r.classify(Unsupported, SelfTransfer);
        return;
    }
    let (Some(a), Some(b), Some(c), Some(d)) = (
        unique_balance(meta.pre_token_balances.as_deref(), src),
        unique_balance(meta.post_token_balances.as_deref(), src),
        unique_balance(meta.pre_token_balances.as_deref(), dst),
        unique_balance(meta.post_token_balances.as_deref(), dst),
    ) else {
        r.classify(NeedsReview, MissingTokenBalances);
        return;
    };
    if [a, b, c, d]
        .iter()
        .any(|v| v.program_id.as_deref() != Some(TOKEN) || v.owner.is_none() || v.mint.is_none())
    {
        r.classify(NeedsReview, MissingOwnerMintOrTokenProgram);
        return;
    }
    if a.owner != b.owner || c.owner != d.owner || [b, c, d].iter().any(|v| v.mint != a.mint) {
        r.classify(Unsupported, InconsistentAccountMetadata);
        return;
    }
    let instruction_amount = if parsed.kind == "transfer" {
        info.amount.as_deref()
    } else {
        info.token_amount.as_ref().and_then(|v| v.amount.as_deref())
    }
    .and_then(atomic);
    let evidence = (|| {
        Some((
            instruction_amount?,
            balance_amount(a)?.checked_sub(balance_amount(b)?)?,
            balance_amount(d)?.checked_sub(balance_amount(c)?)?,
        ))
    })();
    let Some((amount, debit, credit)) = evidence else {
        r.classify(NeedsReview, InvalidAtomicEvidence);
        return;
    };
    if amount != debit
        || amount != credit
        || (parsed.kind == "transferChecked" && info.mint != a.mint)
    {
        r.classify(NeedsReview, InstructionBalanceDisagreement);
        return;
    }
    if info.authority != a.owner
        || !keys
            .iter()
            .any(|key| Some(key.pubkey.as_str()) == a.owner.as_deref() && key.signer == Some(true))
    {
        r.classify(Unsupported, AuthorityNotOwnerSigner);
        return;
    }
    r.evidence=serde_json::json!({"source":source,"destination":destination,"sender_owner":a.owner,"recipient_owner":c.owner,"mint":a.mint,"amount_atomic":amount,"program":TOKEN}).to_string();
    // Structural rejection and explicit execution failures do not require invented
    // finalization evidence; a supported payment can only be classified after it.
    if r.chain_status != ChainStatus::Finalized {
        r.classify(NeedsReview, NotFinalized);
        return;
    }
    let mut differences = Vec::new();
    if a.owner.as_deref() != Some(&p.expected_sender_owner) {
        differences.push("sender_owner");
    }
    if c.owner.as_deref() != Some(&p.expected_recipient_owner) {
        differences.push("recipient_owner");
    }
    if a.mint.as_deref() != Some(&p.expected_mint) {
        differences.push("mint");
    }
    if Some(amount) != atomic(&p.expected_amount_atomic) {
        differences.push("amount_atomic");
    }
    if differences.is_empty() {
        r.classify(Matched, FinalizedDirectTransferVerified);
    } else {
        r.classify(Mismatch, PaymentMismatch);
        r.reason_details = differences.join(";");
    }
}
fn unique_account(keys: &[AccountKey], candidate: Option<&str>) -> Option<usize> {
    let address_value = candidate?;
    if !address(address_value, 32) {
        return None;
    }
    let mut found = keys
        .iter()
        .enumerate()
        .filter(|(_, key)| key.pubkey == address_value);
    let first = found.next()?.0;
    if found.next().is_some() {
        None
    } else {
        Some(first)
    }
}
fn unique_balance(balances: Option<&[TokenBalance]>, index: usize) -> Option<&TokenBalance> {
    let mut found = balances?.iter().filter(|v| v.account_index == Some(index));
    let first = found.next()?;
    if found.next().is_some() {
        None
    } else {
        Some(first)
    }
}
fn balance_amount(balance: &TokenBalance) -> Option<u64> {
    balance
        .ui_token_amount
        .as_ref()?
        .amount
        .as_deref()
        .and_then(atomic)
}
fn ancillary_valid(ix: &Instruction, keys: &[AccountKey], budget_mask: &mut u8) -> bool {
    let Some(program) = ix.program_id.as_deref() else {
        return false;
    };
    if !keys
        .iter()
        .any(|key| key.pubkey == program && key.signer == Some(false))
    {
        return false;
    }
    if program == COMPUTE_BUDGET {
        if ix.parsed.is_some() || !ix.accounts.as_ref().is_some_and(Vec::is_empty) {
            return false;
        }
        let Some(bytes) = ix
            .data
            .as_deref()
            .and_then(|s| bs58::decode(s).into_vec().ok())
        else {
            return false;
        };
        let Some(tag) = bytes.first().copied() else {
            return false;
        };
        let expected = match tag {
            0 | 3 => 9,
            1 | 2 | 4 => 5,
            _ => return false,
        };
        if bytes.len() != expected || *budget_mask & (1 << tag) != 0 {
            return false;
        }
        *budget_mask |= 1 << tag;
        true
    } else {
        match (&ix.parsed, &ix.data, &ix.accounts) {
            (Some(ParsedInstruction::Memo(_)), None, None) => true,
            (None, Some(data), Some(accounts)) => {
                bs58::decode(data)
                    .into_vec()
                    .ok()
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .is_some()
                    && accounts.iter().all(|account| {
                        keys.iter()
                            .any(|key| &key.pubkey == account && key.signer == Some(true))
                    })
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ErrorField, SignatureStatus};
    use serde_json::json;

    fn fixture() -> (Payment, Value) {
        let data: Value = serde_json::from_str(include_str!("../examples/fixtures.json"))
            .expect("checked-in fixture JSON");
        let (signature, value) = data["transactions"]
            .as_object()
            .expect("fixture map")
            .iter()
            .find(|(_, entry)| {
                entry["transaction"]["transaction"]["message"]["instructions"][0]["parsed"]["type"]
                    == "transfer"
                    && entry["transaction"]["meta"]["preTokenBalances"][0]
                        .get("owner")
                        .is_some()
            })
            .expect("direct transfer fixture");
        let tx = value["transaction"].clone();
        let balances = &tx["meta"]["preTokenBalances"];
        let payment = Payment {
            payment_id: "unit-payment".into(),
            tx_signature: signature.clone(),
            expected_sender_owner: balances[0]["owner"].as_str().expect("owner").into(),
            expected_recipient_owner: balances[1]["owner"].as_str().expect("owner").into(),
            expected_mint: balances[0]["mint"].as_str().expect("mint").into(),
            expected_amount_atomic: "100".into(),
        };
        (payment, tx)
    }
    fn evaluate(p: &Payment, tx: &Value) -> Report {
        let mut report = Report::new(1, p);
        report.chain_status = ChainStatus::Finalized;
        reconcile(p, tx, &mut report);
        report
    }
    fn add_program(tx: &mut Value, program: &str) {
        tx["transaction"]["message"]["accountKeys"]
            .as_array_mut()
            .expect("account keys")
            .push(json!({"pubkey":program,"signer":false}));
    }
    fn compute(tag: u8, length: usize) -> Value {
        let mut bytes = vec![0; length];
        if let Some(first) = bytes.first_mut() {
            *first = tag;
        }
        json!({"programId":COMPUTE_BUDGET,"accounts":[],"data":bs58::encode(bytes).into_string()})
    }
    #[test]
    fn direct_transfer_matches_using_integer_evidence_and_block_time() {
        let (p, mut tx) = fixture();
        tx["blockTime"] = json!(1700000000);
        let report = evaluate(&p, &tx);
        assert_eq!(report.reconciliation_status, ReconciliationStatus::Matched);
        assert_eq!(report.reason, Reason::FinalizedDirectTransferVerified);
        assert_eq!(report.block_time, Some(1700000000));
        assert_eq!(report.fee_lamports, Some(5000));
    }
    #[test]
    fn payment_validation_explains_the_invalid_field() {
        let (mut p, _) = fixture();
        assert_eq!(p.validate(), Ok(()));
        p.expected_amount_atomic = "1.0".into();
        assert_eq!(p.validate(), Err(Reason::InvalidAmount));
        p.expected_amount_atomic = "100".into();
        p.expected_recipient_owner = "token-account-name".into();
        assert_eq!(p.validate(), Err(Reason::InvalidRecipientOwner));
    }
    #[test]
    fn atomic_amount_has_strict_u64_boundaries() {
        assert_eq!(atomic("18446744073709551615"), Some(u64::MAX));
        for invalid in [
            "18446744073709551616",
            "01",
            "+1",
            "-1",
            "1.0",
            "1e3",
            " 1",
            "",
        ] {
            assert_eq!(atomic(invalid), None, "{invalid}");
        }
    }
    #[test]
    fn mismatches_have_stable_reason_and_separate_details() {
        let (mut p, tx) = fixture();
        p.expected_amount_atomic = "101".into();
        let report = evaluate(&p, &tx);
        assert_eq!(report.reconciliation_status, ReconciliationStatus::Mismatch);
        assert_eq!(report.reason, Reason::PaymentMismatch);
        assert_eq!(report.reason_details, "amount_atomic");
        assert_eq!(
            serde_json::to_value(report).expect("report JSON")["reason"],
            "payment_mismatch"
        );
    }
    #[test]
    fn compute_budget_and_both_memo_programs_surround_direct_transfer() {
        for memo in [MEMO, MEMO_LEGACY] {
            let (p, mut tx) = fixture();
            add_program(&mut tx, COMPUTE_BUDGET);
            add_program(&mut tx, memo);
            let ixs = tx["transaction"]["message"]["instructions"]
                .as_array_mut()
                .expect("instructions");
            ixs.insert(0, compute(2, 5));
            ixs.insert(1, compute(3, 9));
            ixs.push(json!({"programId":memo,"parsed":"invoice:unit-payment"}));
            assert_eq!(
                evaluate(&p, &tx).reconciliation_status,
                ReconciliationStatus::Matched
            );
        }
    }
    #[test]
    fn raw_memo_accepts_utf8_and_rejects_non_signer_accounts() {
        let (p, mut tx) = fixture();
        add_program(&mut tx, MEMO);
        let memo = json!({"programId":MEMO,"accounts":[],"data":bs58::encode("payment memo").into_string()});
        tx["transaction"]["message"]["instructions"]
            .as_array_mut()
            .expect("instructions")
            .push(memo);
        assert_eq!(
            evaluate(&p, &tx).reconciliation_status,
            ReconciliationStatus::Matched
        );
        let nonsigner = tx["transaction"]["message"]["accountKeys"][0]["pubkey"].clone();
        tx["transaction"]["message"]["instructions"][1]["accounts"] = json!([nonsigner]);
        assert_eq!(
            evaluate(&p, &tx).reason,
            Reason::UnsupportedAncillaryInstruction
        );
    }
    #[test]
    fn malformed_compute_budget_is_never_ignored() {
        for (tag, len) in [(2, 4), (2, 6), (3, 5), (5, 5)] {
            let (p, mut tx) = fixture();
            add_program(&mut tx, COMPUTE_BUDGET);
            tx["transaction"]["message"]["instructions"]
                .as_array_mut()
                .expect("instructions")
                .insert(0, compute(tag, len));
            assert_eq!(
                evaluate(&p, &tx).reason,
                Reason::UnsupportedAncillaryInstruction,
                "tag={tag},len={len}"
            );
        }
    }
    #[test]
    fn unknown_program_cannot_impersonate_a_memo() {
        let (p, mut tx) = fixture();
        tx["transaction"]["message"]["instructions"]
            .as_array_mut()
            .expect("instructions")
            .push(json!({"programId":"router","program":"spl-memo","parsed":"invoice"}));
        assert_ne!(
            evaluate(&p, &tx).reconciliation_status,
            ReconciliationStatus::Matched
        );
    }
    #[test]
    fn duplicate_transfers_or_cpi_stay_unsupported() {
        let (p, mut tx) = fixture();
        let transfer = tx["transaction"]["message"]["instructions"][0].clone();
        tx["transaction"]["message"]["instructions"]
            .as_array_mut()
            .expect("instructions")
            .push(transfer);
        assert_eq!(
            evaluate(&p, &tx).reconciliation_status,
            ReconciliationStatus::Unsupported
        );
        let (p, mut tx) = fixture();
        tx["meta"]["innerInstructions"] = json!([{"index":0,"instructions":[]}]);
        assert_eq!(
            evaluate(&p, &tx).reconciliation_status,
            ReconciliationStatus::Unsupported
        );
    }
    #[test]
    fn missing_owner_or_ambiguous_account_index_never_matches() {
        let (p, mut tx) = fixture();
        tx["meta"]["preTokenBalances"][0]
            .as_object_mut()
            .expect("balance")
            .remove("owner");
        assert_eq!(
            evaluate(&p, &tx).reason,
            Reason::MissingOwnerMintOrTokenProgram
        );
        let (p, mut tx) = fixture();
        let balance = tx["meta"]["preTokenBalances"][0].clone();
        tx["meta"]["preTokenBalances"]
            .as_array_mut()
            .expect("balances")
            .push(balance);
        assert_eq!(evaluate(&p, &tx).reason, Reason::MissingTokenBalances);
    }
    #[test]
    fn no_confirmation_or_no_execution_evidence_is_not_success() {
        for (json, expected, success) in [
            (
                json!({"slot":123,"confirmationStatus":"finalized"}),
                ChainStatus::Unknown,
                false,
            ),
            (
                json!({"slot":123,"err":false,"confirmationStatus":"finalized"}),
                ChainStatus::Unknown,
                false,
            ),
            (
                json!({"slot":123,"err":null,"confirmationStatus":"confirmed"}),
                ChainStatus::Confirmed,
                true,
            ),
            (
                json!({"slot":123,"err":{"InstructionError":[0,"failure"]},"confirmationStatus":"finalized"}),
                ChainStatus::Failed,
                false,
            ),
        ] {
            let status: SignatureStatus = serde_json::from_value(json).expect("typed status");
            assert_eq!(status.chain_status(), expected);
            assert_eq!(status.succeeded(), success);
        }
        let missing: SignatureStatus = serde_json::from_value(json!({})).expect("typed status");
        assert_eq!(missing.err, ErrorField::Missing);
    }
    #[test]
    fn library_gate_requires_finalized_and_matching_identity() {
        let (p, tx) = fixture();
        let mut report = Report::new(1, &p);
        reconcile(&p, &tx, &mut report);
        assert_eq!(report.reason, Reason::NotFinalized);
        let (p, mut tx) = fixture();
        tx["transaction"]["signatures"][0] = json!("different");
        assert_eq!(
            evaluate(&p, &tx).reason,
            Reason::TransactionIdentityOrSlotDisagreement
        );
    }
    #[test]
    fn zero_delta_cannot_match_positive_expected_amount() {
        let (p, mut tx) = fixture();
        tx["meta"]["postTokenBalances"][1]["uiTokenAmount"]["amount"] = json!("0");
        assert_eq!(
            evaluate(&p, &tx).reason,
            Reason::InstructionBalanceDisagreement
        );
    }
    #[test]
    fn unfinalized_details_retain_metadata_and_still_reject_unknown_effects() {
        let (p, mut tx) = fixture();
        tx["blockTime"] = json!(1700000000);
        tx["transaction"]["message"]["instructions"]
            .as_array_mut()
            .expect("instructions")
            .push(json!({"programId":"unknown-router"}));
        let mut report = Report::new(1, &p);
        reconcile(&p, &tx, &mut report);
        assert_eq!(
            report.reconciliation_status,
            ReconciliationStatus::Unsupported
        );
        assert_eq!(report.chain_status, ChainStatus::Unknown);
        assert_eq!(report.slot, Some(123));
        assert_eq!(report.block_time, Some(1700000000));
        assert_eq!(report.fee_lamports, Some(5000));
    }
    #[test]
    fn execution_failure_retains_metadata_without_inventing_finalization() {
        let (p, mut tx) = fixture();
        tx["blockTime"] = json!(1700000000);
        tx["meta"]["err"] = json!({"InstructionError":[0,"InvalidArgument"]});
        let mut report = Report::new(1, &p);
        reconcile(&p, &tx, &mut report);
        assert_eq!(report.chain_status, ChainStatus::Failed);
        assert_eq!(
            report.reconciliation_status,
            ReconciliationStatus::NeedsReview
        );
        assert_eq!(report.reason, Reason::TransactionExecutionErrorOrMissing);
        assert_eq!(report.slot, Some(123));
        assert_eq!(report.block_time, Some(1700000000));
    }
}
