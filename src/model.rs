//! Typed subset of Solana's jsonParsed RPC responses. Unknown extra fields are ignored.
use crate::domain::ChainStatus;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Distinguishes omitted execution status from explicit successful `err: null`.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ErrorField {
    #[default]
    Missing,
    Null,
    Error(Value),
    Invalid,
}
impl<'de> Deserialize<'de> for ErrorField {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(match value {
            Value::Null => Self::Null,
            Value::String(_) | Value::Object(_) => Self::Error(value),
            _ => Self::Invalid,
        })
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct SignatureStatuses {
    pub value: Vec<Option<SignatureStatus>>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureStatus {
    pub slot: Option<u64>,
    #[serde(default)]
    pub err: ErrorField,
    pub confirmation_status: Option<String>,
}
impl SignatureStatus {
    pub fn execution_error(&self) -> Option<&Value> {
        match &self.err {
            ErrorField::Error(value) => Some(value),
            _ => None,
        }
    }
    pub fn succeeded(&self) -> bool {
        self.err == ErrorField::Null
    }
    pub fn chain_status(&self) -> ChainStatus {
        if self.execution_error().is_some() {
            return ChainStatus::Failed;
        }
        if !self.succeeded() {
            return ChainStatus::Unknown;
        }
        ChainStatus::from_name(self.confirmation_status.as_deref())
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum TransactionVersion {
    Text(String),
    Number(u64),
}
impl TransactionVersion {
    pub fn supported(&self) -> bool {
        matches!(self, Self::Text(value) if value == "legacy") || matches!(self, Self::Number(0))
    }
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transaction {
    pub slot: Option<u64>,
    pub block_time: Option<i64>,
    pub version: Option<TransactionVersion>,
    pub transaction: Option<TransactionData>,
    pub meta: Option<TransactionMeta>,
}
impl Transaction {
    pub fn parse(value: Value) -> serde_json::Result<Self> {
        serde_json::from_value(value)
    }
    pub fn first_signature(&self) -> Option<&str> {
        self.transaction
            .as_ref()?
            .signatures
            .as_ref()?
            .first()
            .map(String::as_str)
    }
}
#[derive(Clone, Debug, Deserialize)]
pub struct TransactionData {
    pub signatures: Option<Vec<String>>,
    pub message: Option<Message>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub account_keys: Option<Vec<AccountKey>>,
    pub instructions: Option<Vec<Instruction>>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct AccountKey {
    pub pubkey: String,
    pub signer: Option<bool>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionMeta {
    #[serde(default)]
    pub err: ErrorField,
    pub fee: Option<u64>,
    pub inner_instructions: Option<Vec<Value>>,
    pub pre_token_balances: Option<Vec<TokenBalance>>,
    pub post_token_balances: Option<Vec<TokenBalance>>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenBalance {
    pub account_index: Option<usize>,
    pub mint: Option<String>,
    pub owner: Option<String>,
    pub program_id: Option<String>,
    pub ui_token_amount: Option<TokenAmount>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct TokenAmount {
    pub amount: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Instruction {
    pub program_id: Option<String>,
    pub parsed: Option<ParsedInstruction>,
    pub data: Option<String>,
    pub accounts: Option<Vec<String>>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum ParsedInstruction {
    Token(TokenInstruction),
    Memo(String),
    Unknown(Value),
}
#[derive(Clone, Debug, Deserialize)]
pub struct TokenInstruction {
    #[serde(rename = "type")]
    pub kind: String,
    pub info: Option<TokenInfo>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenInfo {
    pub source: Option<String>,
    pub destination: Option<String>,
    pub authority: Option<String>,
    pub amount: Option<String>,
    pub mint: Option<String>,
    pub token_amount: Option<TokenAmount>,
}
