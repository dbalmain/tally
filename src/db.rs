use rusqlite::Connection;

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
"#;

pub(crate) fn init_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA)?;
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
    }
}
