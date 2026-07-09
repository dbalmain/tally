//! Named SQL placeholders supported by search rendering.

pub const DATE: &str = "date";
pub const TRANSACTION_ID: &str = "transaction_id";
pub const AMOUNT_CENTS: &str = "amount_cents";
pub const BALANCE_CENTS: &str = "balance_cents";
pub const DESCRIPTION: &str = "description";
pub const BANK_NAME: &str = "bank_name";
pub const ACCOUNT_NAME: &str = "account_name";
pub const CATEGORY_PATH: &str = "category_path";
pub const FTS_MATCH: &str = "fts_match";
pub const FTS_NOT_MATCH: &str = "fts_not_match";

pub fn reference(name: &str) -> String {
    format!("{{{name}}}")
}
