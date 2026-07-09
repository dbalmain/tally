//! Shared test fixtures and DB round-trip assertion helpers for the store
//! submodule tests.

use chrono::{NaiveDate, Weekday};
use rusqlite::params;
use std::collections::HashMap;
use std::fs;
use tempfile::TempDir;

use crate::search::{ParsedQuery, SearchConfig, SearchOptions, parse};
use crate::{
    Category, CategorySource, Transaction, TransactionEnrichment, Transfer, TransferSource,
};

use super::{TransactionStore, parse_datetime};

/// Build a SearchConfig with all standard filters and no completion options.
fn search_config() -> SearchConfig {
    SearchConfig::standard(
        vec![],
        Some(vec![]),
        SearchOptions::new(
            NaiveDate::from_ymd_opt(2026, 7, 9).unwrap(),
            Weekday::Mon,
            (6, 30),
        ),
    )
}

/// Convenience: parse a query string with the standard search config.
pub(crate) fn q(input: &str) -> ParsedQuery {
    let (parsed, _) = parse(&search_config(), input, input.chars().count());
    parsed
}

/// Strip the cursor's implicit `*` so FTS tests get exact matching.
/// `parse()` adds a `*` at the end if the cursor is at the end of the FTS
/// text — handy in the TUI but a foot-gun in unit tests.
pub(crate) fn q_exact(input: &str) -> ParsedQuery {
    // Pass cursor=0 so no implicit prefix is added.
    let (parsed, _) = parse(&search_config(), input, 0);
    parsed
}

pub(crate) fn annotate_transaction(
    store: &TransactionStore,
    description: &str,
    metadata: &str,
    source_file: &str,
    hash: &str,
) {
    store
        .conn
        .execute(
            "UPDATE transactions
             SET metadata = ?, source_file = ?, hash = ?
             WHERE description = ?",
            params![metadata, source_file, hash, description],
        )
        .unwrap();
}

pub(crate) fn assert_transaction_matches_db(store: &TransactionStore, tx: &Transaction) {
    let (
        bank_id,
        account_id,
        date_str,
        description,
        amount_cents,
        balance_cents,
        hash,
        metadata_json,
        source_file,
        import_batch_id,
    ): (
        i64,
        i64,
        String,
        String,
        i64,
        i64,
        String,
        String,
        String,
        i64,
    ) = store
        .conn
        .query_row(
            "SELECT bank_id, account_id, date, description, amount_cents,
                    balance_cents, hash, metadata, source_file, import_batch_id
             FROM transactions_view
             WHERE id = ?",
            [tx.id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .unwrap();

    let metadata: HashMap<String, serde_json::Value> =
        serde_json::from_str(&metadata_json).unwrap();

    assert_eq!(tx.bank_id, bank_id);
    assert_eq!(tx.account_id, account_id);
    assert_eq!(
        tx.date,
        NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").unwrap()
    );
    assert_eq!(tx.description, description);
    assert_eq!(tx.amount_cents, amount_cents);
    assert_eq!(tx.balance_cents, balance_cents);
    assert_eq!(tx.hash, hash);
    assert_eq!(tx.metadata, metadata);
    assert_eq!(tx.source_file, source_file);
    assert_eq!(tx.import_batch_id, import_batch_id);
}

pub(crate) fn assert_enrichment_matches_db(
    store: &TransactionStore,
    enrichment: &TransactionEnrichment,
) {
    let (
        transaction_id,
        category_id,
        category_source,
        category_confirmed,
        ai_confidence,
        created_at,
        updated_at,
    ): (
        i64,
        Option<i64>,
        Option<String>,
        i32,
        Option<f64>,
        String,
        String,
    ) = store
        .conn
        .query_row(
            "SELECT transaction_id, category_id, category_source,
                    category_confirmed, ai_confidence, created_at, updated_at
             FROM transaction_enrichments
             WHERE id = ?",
            [enrichment.id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .unwrap();

    assert_eq!(enrichment.transaction_id, transaction_id);
    assert_eq!(enrichment.category_id, category_id);
    assert_eq!(
        enrichment.category_source,
        category_source.and_then(|source| source.parse().ok())
    );
    assert_eq!(enrichment.category_confirmed, category_confirmed != 0);
    assert_eq!(enrichment.ai_confidence, ai_confidence);
    assert_eq!(enrichment.created_at, parse_datetime(&created_at).unwrap());
    assert_eq!(enrichment.updated_at, parse_datetime(&updated_at).unwrap());
}

pub(crate) fn assert_category_matches_db(store: &TransactionStore, category: &Category) {
    let (path, created_at): (String, String) = store
        .conn
        .query_row(
            "SELECT path, created_at FROM categories WHERE id = ?",
            [category.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();

    assert_eq!(category.path, path);
    assert_eq!(category.created_at, parse_datetime(&created_at).unwrap());
}

pub(crate) fn assert_transfer_matches_db(store: &TransactionStore, transfer: &Transfer) {
    let (from_transaction_id, to_transaction_id, source, confirmed, ai_confidence, created_at): (
        i64,
        i64,
        String,
        i32,
        Option<f64>,
        String,
    ) = store
        .conn
        .query_row(
            "SELECT from_transaction_id, to_transaction_id, source, confirmed,
                    ai_confidence, created_at
             FROM transfers
             WHERE id = ?",
            [transfer.id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .unwrap();

    assert_eq!(transfer.from_transaction_id, from_transaction_id);
    assert_eq!(transfer.to_transaction_id, to_transaction_id);
    assert_eq!(transfer.source, source.parse().unwrap());
    assert_eq!(transfer.confirmed, confirmed != 0);
    assert_eq!(transfer.ai_confidence, ai_confidence);
    assert_eq!(transfer.created_at, parse_datetime(&created_at).unwrap());
}

pub(crate) fn setup_test_exports() -> TempDir {
    let temp = TempDir::new().unwrap();
    let bank_dir = temp.path().join("TestBank");
    let account_dir = bank_dir.join("Checking");
    fs::create_dir_all(&account_dir).unwrap();

    fs::write(
        account_dir.join("transactions.csv"),
        "Date,Description,Amount,Balance\n2025-01-01,Test,-100,500\n",
    )
    .unwrap();

    let import_script = account_dir.join("import");
    fs::write(
        &import_script,
        r#"#!/usr/bin/env bash
echo '[{"date":"2025-01-01","description":"Test transaction","amount_cents":-10000,"balance_cents":50000}]'
"#,
    )
    .unwrap();

    make_executable(&import_script);

    temp
}

fn make_executable(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(not(unix))]
    let _ = path;
}

pub(crate) fn write_pull_script(path: &std::path::Path, description: &str, hash: &str) {
    fs::write(
        path,
        format!(
            r#"#!/usr/bin/env bash
echo '[{{"date":"2025-01-01","description":"{description}","amount_cents":-10000,"balance_cents":50000,"hash":"{hash}"}}]'
"#
        ),
    )
    .unwrap();
    make_executable(path);
}

/// Test fixture with two banks, three accounts, several transactions,
/// some categorised, and one transfer pair. Used to exercise rendering
/// across multiple filter types and store query methods.
pub(crate) fn setup_rich_fixture() -> (TempDir, TransactionStore) {
    let temp = TempDir::new().unwrap();
    let store = TransactionStore::open_in_memory(temp.path()).unwrap();

    // Insert banks/accounts directly to avoid relying on import scripts.
    store
        .conn
        .execute("INSERT INTO banks (name) VALUES ('ING')", [])
        .unwrap();
    store
        .conn
        .execute("INSERT INTO banks (name) VALUES ('NAB')", [])
        .unwrap();
    let ing_id: i64 = store
        .conn
        .query_row("SELECT id FROM banks WHERE name='ING'", [], |r| r.get(0))
        .unwrap();
    let nab_id: i64 = store
        .conn
        .query_row("SELECT id FROM banks WHERE name='NAB'", [], |r| r.get(0))
        .unwrap();

    store
        .conn
        .execute(
            "INSERT INTO accounts (bank_id, name) VALUES (?, 'Orange Everyday')",
            [ing_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO accounts (bank_id, name) VALUES (?, 'Savings Maximiser')",
            [ing_id],
        )
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO accounts (bank_id, name) VALUES (?, 'Classic')",
            [nab_id],
        )
        .unwrap();
    let ing_orange: i64 = store
        .conn
        .query_row(
            "SELECT id FROM accounts WHERE name='Orange Everyday'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let ing_savings: i64 = store
        .conn
        .query_row(
            "SELECT id FROM accounts WHERE name='Savings Maximiser'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let nab_classic: i64 = store
        .conn
        .query_row("SELECT id FROM accounts WHERE name='Classic'", [], |r| {
            r.get(0)
        })
        .unwrap();

    // Make a batch.
    store
        .conn
        .execute(
            "INSERT INTO import_batches (started_at) VALUES ('2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    let batch_id: i64 = store.conn.last_insert_rowid();

    // Insert a handful of transactions.
    let txs = [
        (ing_orange, "2024-01-15", "Coffee Shop", -500i64, 100000i64),
        (ing_orange, "2024-02-20", "Grocery Store", -8500, 91500),
        (ing_orange, "2024-03-10", "Salary", 250000, 341500),
        (ing_savings, "2024-03-15", "Interest", 1234, 50000),
        (nab_classic, "2024-04-05", "Coffee Bean", -750, 75000),
        // Transfer pair (equal & opposite)
        (ing_orange, "2024-05-01", "Transfer Out", -10000, 331500),
        (nab_classic, "2024-05-01", "Transfer In", 10000, 85000),
    ];
    for (account_id, date, desc, amount, balance) in txs {
        store
            .conn
            .execute(
                "INSERT INTO transactions
                 (account_id, date, description, amount_cents, balance_cents,
                  hash, metadata, source_file, import_batch_id)
                 VALUES (?, ?, ?, ?, ?, ?, '{}', '', ?)",
                params![
                    account_id,
                    date,
                    desc,
                    amount,
                    balance,
                    format!("{}-{}-{}", account_id, date, amount),
                    batch_id,
                ],
            )
            .unwrap();
        let tx_id = store.conn.last_insert_rowid();
        store
            .conn
            .execute(
                "INSERT INTO transactions_fts (rowid, searchable_text) VALUES (?, ?)",
                params![tx_id, desc],
            )
            .unwrap();
    }

    // Categorise some.
    let mut store = store; // need mut for set_category
    let food_id = store.get_or_create_category("Food/Groceries").unwrap();
    let coffee_id = store.get_or_create_category("Food/Coffee").unwrap();
    let income_id = store.get_or_create_category("Income/Salary").unwrap();

    let id_of = |store: &TransactionStore, desc: &str| -> i64 {
        store
            .conn
            .query_row(
                "SELECT id FROM transactions WHERE description = ? LIMIT 1",
                [desc],
                |r| r.get(0),
            )
            .unwrap()
    };

    let coffee_tx = id_of(&store, "Coffee Shop");
    let groc_tx = id_of(&store, "Grocery Store");
    let salary_tx = id_of(&store, "Salary");
    let coffee_bean_tx = id_of(&store, "Coffee Bean");

    store
        .set_category(coffee_tx, coffee_id, CategorySource::Manual, true, None)
        .unwrap();
    store
        .set_category(groc_tx, food_id, CategorySource::Manual, true, None)
        .unwrap();
    store
        .set_category(salary_tx, income_id, CategorySource::Manual, true, None)
        .unwrap();
    // AI-suggested, awaiting review:
    store
        .set_category(
            coffee_bean_tx,
            coffee_id,
            CategorySource::Ai,
            false,
            Some(0.85),
        )
        .unwrap();

    // Create the transfer link.
    let xfer_out = id_of(&store, "Transfer Out");
    let xfer_in = id_of(&store, "Transfer In");
    store
        .create_transfer(xfer_out, xfer_in, TransferSource::Manual, true, None)
        .unwrap();

    (temp, store)
}

/// Build a small store with two accounts and a controllable set of
/// transactions, so transfer-candidate behavior can be exercised directly.
pub(crate) fn store_with_two_accounts() -> (TempDir, TransactionStore, i64, i64) {
    let temp = TempDir::new().unwrap();
    let store = TransactionStore::open_in_memory(temp.path()).unwrap();
    store
        .conn
        .execute("INSERT INTO banks (name) VALUES ('TB')", [])
        .unwrap();
    let bank_id: i64 = store.conn.last_insert_rowid();
    store
        .conn
        .execute(
            "INSERT INTO accounts (bank_id, name) VALUES (?, 'A1')",
            [bank_id],
        )
        .unwrap();
    let a1: i64 = store.conn.last_insert_rowid();
    store
        .conn
        .execute(
            "INSERT INTO accounts (bank_id, name) VALUES (?, 'A2')",
            [bank_id],
        )
        .unwrap();
    let a2: i64 = store.conn.last_insert_rowid();
    store
        .conn
        .execute(
            "INSERT INTO import_batches (started_at) VALUES ('2024-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
    (temp, store, a1, a2)
}

pub(crate) fn insert_tx(store: &TransactionStore, account_id: i64, date: &str, amount: i64) -> i64 {
    insert_tx_desc(store, account_id, date, "tx", amount)
}

/// Like [`insert_tx`] but with a caller-chosen description, indexed for FTS so
/// full-text filter queries can match it.
pub(crate) fn insert_tx_desc(
    store: &TransactionStore,
    account_id: i64,
    date: &str,
    description: &str,
    amount: i64,
) -> i64 {
    let batch_id: i64 = store
        .conn
        .query_row("SELECT id FROM import_batches LIMIT 1", [], |r| r.get(0))
        .unwrap();
    store
        .conn
        .execute(
            "INSERT INTO transactions
             (account_id, date, description, amount_cents, balance_cents,
              hash, metadata, source_file, import_batch_id)
             VALUES (?, ?, ?, ?, 0, ?, '{}', '', ?)",
            params![
                account_id,
                date,
                description,
                amount,
                format!("{account_id}-{date}-{description}-{amount}"),
                batch_id
            ],
        )
        .unwrap();
    let tx_id = store.conn.last_insert_rowid();
    store
        .conn
        .execute(
            "INSERT INTO transactions_fts (rowid, searchable_text) VALUES (?, ?)",
            params![tx_id, description],
        )
        .unwrap();
    tx_id
}

pub(crate) fn get_tx(store: &TransactionStore, id: i64) -> Transaction {
    store.get_transaction_by_id(id).unwrap().unwrap()
}
