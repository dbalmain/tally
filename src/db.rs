use regex::Regex;
use rusqlite::Connection;
use rusqlite::functions::FunctionFlags;

use crate::Result;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS banks (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    deleted_at TEXT
);

CREATE TABLE IF NOT EXISTS accounts (
    id INTEGER PRIMARY KEY,
    bank_id INTEGER NOT NULL REFERENCES banks(id),
    name TEXT NOT NULL,
    deleted_at TEXT,
    UNIQUE(bank_id, name)
);

CREATE TABLE IF NOT EXISTS transactions (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    date TEXT NOT NULL,
    description TEXT NOT NULL,
    amount_cents INTEGER NOT NULL,
    balance_cents INTEGER NOT NULL,
    hash TEXT NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}',
    source_file TEXT NOT NULL,
    import_batch_id INTEGER NOT NULL,
    UNIQUE(account_id, hash)
);

CREATE TABLE IF NOT EXISTS imported_files (
    id INTEGER PRIMARY KEY,
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    path TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    imported_at TEXT NOT NULL,
    import_batch_id INTEGER NOT NULL,
    UNIQUE(account_id, content_hash)
);

CREATE TABLE IF NOT EXISTS import_batches (
    id INTEGER PRIMARY KEY,
    started_at TEXT NOT NULL,
    completed_at TEXT
);

CREATE TABLE IF NOT EXISTS categories (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS transaction_enrichments (
    id INTEGER PRIMARY KEY,
    transaction_id INTEGER NOT NULL UNIQUE REFERENCES transactions(id),
    category_id INTEGER REFERENCES categories(id),
    category_source TEXT,
    category_confirmed INTEGER NOT NULL DEFAULT 0,
    ai_confidence REAL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS transfers (
    id INTEGER PRIMARY KEY,
    from_transaction_id INTEGER NOT NULL UNIQUE REFERENCES transactions(id),
    to_transaction_id INTEGER NOT NULL UNIQUE REFERENCES transactions(id),
    source TEXT NOT NULL,
    confirmed INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_transactions_account_date ON transactions(account_id, date);
CREATE INDEX IF NOT EXISTS idx_transactions_hash ON transactions(hash);
CREATE INDEX IF NOT EXISTS idx_accounts_bank ON accounts(bank_id);
CREATE INDEX IF NOT EXISTS idx_enrichments_category ON transaction_enrichments(category_id);
CREATE INDEX IF NOT EXISTS idx_enrichments_confirmed ON transaction_enrichments(category_confirmed);
CREATE INDEX IF NOT EXISTS idx_transfers_confirmed ON transfers(confirmed);

-- FTS5 virtual table for full-text search on transactions
-- contentless: we don't duplicate data, just index it
-- contentless_delete=1: allows DELETE operations
CREATE VIRTUAL TABLE IF NOT EXISTS transactions_fts USING fts5(
    searchable_text,
    content='',
    contentless_delete=1
);

-- Read-side view: a transaction joined to its account and bank. All store
-- read queries go through this view so the join (and the bank/account name
-- columns search filters rely on) is defined in exactly one place.
-- Dropped and recreated on every open so definition changes take effect
-- without migrations. The leading columns (id..import_batch_id) must stay in
-- the order store::parse_transaction_at_offset expects.
DROP VIEW IF EXISTS transactions_view;
CREATE VIEW transactions_view AS
SELECT
    t.id, a.bank_id, t.account_id, t.date, t.description,
    t.amount_cents, t.balance_cents, t.hash, t.metadata,
    t.source_file, t.import_batch_id,
    b.name AS bank_name,
    a.name AS account_name,
    a.deleted_at AS account_deleted_at
FROM transactions t
JOIN accounts a ON t.account_id = a.id
JOIN banks b ON a.bank_id = b.id;
"#;

pub(crate) fn init_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
    register_regexp_function(conn)?;
    Ok(())
}

/// Build searchable text from description and metadata for FTS indexing.
/// Flattens all string and number values from metadata.
pub(crate) fn build_searchable_text(
    description: &str,
    metadata: &std::collections::HashMap<String, serde_json::Value>,
) -> String {
    let mut parts = vec![description.to_string()];

    for value in metadata.values() {
        flatten_json_value(value, &mut parts);
    }

    parts.join(" ")
}

fn flatten_json_value(value: &serde_json::Value, parts: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => parts.push(s.clone()),
        serde_json::Value::Number(n) => parts.push(n.to_string()),
        serde_json::Value::Array(arr) => {
            for item in arr {
                flatten_json_value(item, parts);
            }
        }
        serde_json::Value::Object(obj) => {
            for v in obj.values() {
                flatten_json_value(v, parts);
            }
        }
        _ => {}
    }
}

fn register_regexp_function(conn: &Connection) -> Result<()> {
    conn.create_scalar_function("regexp", 2, FunctionFlags::SQLITE_DETERMINISTIC, |ctx| {
        // Use SQLite's auxiliary data to cache the compiled regex.
        // When the pattern (arg 0) is constant across rows, SQLite preserves the cache.
        let re = ctx.get_or_create_aux(0, |vr| -> std::result::Result<Regex, rusqlite::Error> {
            let pattern = vr
                .as_str()
                .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?;
            Regex::new(pattern).map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))
        })?;
        let text: String = ctx.get(1)?;
        Ok(re.is_match(&text))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_db() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();

        assert!(tables.contains(&"banks".to_string()));
        assert!(tables.contains(&"accounts".to_string()));
        assert!(tables.contains(&"transactions".to_string()));
        assert!(tables.contains(&"imported_files".to_string()));
        assert!(tables.contains(&"transactions_fts".to_string()));
    }

    #[test]
    fn test_regexp_function() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Test matching
        let result: bool = conn
            .query_row("SELECT regexp('cof.*fee', 'I love coffee')", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(result);

        // Test non-matching
        let result: bool = conn
            .query_row("SELECT regexp('tea', 'I love coffee')", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(!result);

        // Test case-insensitive with (?i)
        let result: bool = conn
            .query_row("SELECT regexp('(?i)COFFEE', 'I love coffee')", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(result);
    }

    #[test]
    fn test_fts5_basic() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Insert test data
        conn.execute(
            "INSERT INTO transactions_fts (rowid, searchable_text) VALUES (1, 'AAMI Insurance March payment')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO transactions_fts (rowid, searchable_text) VALUES (2, 'Coffee shop purchase')",
            [],
        )
        .unwrap();

        // Term search (implicit AND)
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transactions_fts WHERE transactions_fts MATCH 'AAMI March'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Case insensitive
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transactions_fts WHERE transactions_fts MATCH 'aami march'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Prefix match
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transactions_fts WHERE transactions_fts MATCH 'mar*'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Phrase match
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transactions_fts WHERE transactions_fts MATCH '\"Coffee shop\"'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // OR query
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM transactions_fts WHERE transactions_fts MATCH '(AAMI) OR (Coffee)'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_build_searchable_text() {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "merchant".to_string(),
            serde_json::Value::String("Woolworths".to_string()),
        );
        metadata.insert("amount".to_string(), serde_json::json!(42.50));

        let text = build_searchable_text("Grocery purchase", &metadata);
        assert!(text.contains("Grocery purchase"));
        assert!(text.contains("Woolworths"));
        assert!(text.contains("42.5"));
    }

    #[test]
    fn test_build_searchable_text_nested() {
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "details".to_string(),
            serde_json::json!({"category": "food", "tags": ["organic", "local"]}),
        );

        let text = build_searchable_text("Purchase", &metadata);
        assert!(text.contains("Purchase"));
        assert!(text.contains("food"));
        assert!(text.contains("organic"));
        assert!(text.contains("local"));
    }
}
