//! Stable machine-readable reason codes; human context lives in `reason_details`.
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    #[default]
    Unclassified,
    #[serde(rename = "invalid_address_signature_or_atomic_amount")]
    InvalidInput,
    InvalidPaymentId,
    InvalidSignature,
    InvalidSenderOwner,
    InvalidRecipientOwner,
    InvalidMint,
    InvalidAmount,
    #[serde(rename = "signature_not_found_history_may_be_unavailable")]
    SignatureNotFound,
    StatusErrorOrMissing,
    MissingStatusSlot,
    NotFinalized,
    TransactionIdentityOrSlotDisagreement,
    TransactionUnavailable,
    TransactionVersion,
    MissingMeta,
    TransactionExecutionErrorOrMissing,
    MissingInstructions,
    ComplexOrMissingInnerInstructions,
    NonStandardTokenTransfer,
    MissingAccounts,
    TokenProgramAccountMissing,
    SourceAccountMissing,
    DestinationAccountMissing,
    SelfTransfer,
    MissingTokenBalances,
    MissingOwnerMintOrTokenProgram,
    InconsistentAccountMetadata,
    InvalidAtomicEvidence,
    InstructionBalanceDisagreement,
    AuthorityNotOwnerSigner,
    FinalizedDirectTransferVerified,
    PaymentMismatch,
    SignatureReuse,
    PaymentIdReuse,
    UnsupportedAncillaryInstruction,
    MalformedTransaction,
    InfrastructureError,
}
impl Reason {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unclassified => "unclassified",
            Self::InvalidInput => "invalid_address_signature_or_atomic_amount",
            Self::InvalidPaymentId => "invalid_payment_id",
            Self::InvalidSignature => "invalid_signature",
            Self::InvalidSenderOwner => "invalid_sender_owner",
            Self::InvalidRecipientOwner => "invalid_recipient_owner",
            Self::InvalidMint => "invalid_mint",
            Self::InvalidAmount => "invalid_amount",
            Self::SignatureNotFound => "signature_not_found_history_may_be_unavailable",
            Self::StatusErrorOrMissing => "status_error_or_missing",
            Self::MissingStatusSlot => "missing_status_slot",
            Self::NotFinalized => "not_finalized",
            Self::TransactionIdentityOrSlotDisagreement => {
                "transaction_identity_or_slot_disagreement"
            }
            Self::TransactionUnavailable => "transaction_unavailable",
            Self::TransactionVersion => "transaction_version",
            Self::MissingMeta => "missing_meta",
            Self::TransactionExecutionErrorOrMissing => "transaction_execution_error_or_missing",
            Self::MissingInstructions => "missing_instructions",
            Self::ComplexOrMissingInnerInstructions => "complex_or_missing_inner_instructions",
            Self::NonStandardTokenTransfer => "non_standard_token_transfer",
            Self::MissingAccounts => "missing_accounts",
            Self::TokenProgramAccountMissing => "token_program_account_missing",
            Self::SourceAccountMissing => "source_account_missing",
            Self::DestinationAccountMissing => "destination_account_missing",
            Self::SelfTransfer => "self_transfer",
            Self::MissingTokenBalances => "missing_token_balances",
            Self::MissingOwnerMintOrTokenProgram => "missing_owner_mint_or_token_program",
            Self::InconsistentAccountMetadata => "inconsistent_account_metadata",
            Self::InvalidAtomicEvidence => "invalid_atomic_evidence",
            Self::InstructionBalanceDisagreement => "instruction_balance_disagreement",
            Self::AuthorityNotOwnerSigner => "authority_not_owner_signer",
            Self::FinalizedDirectTransferVerified => "finalized_direct_transfer_verified",
            Self::PaymentMismatch => "payment_mismatch",
            Self::SignatureReuse => "signature_reuse",
            Self::PaymentIdReuse => "payment_id_reuse",
            Self::UnsupportedAncillaryInstruction => "unsupported_ancillary_instruction",
            Self::MalformedTransaction => "malformed_transaction",
            Self::InfrastructureError => "infrastructure_error",
        }
    }
}
impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}
