# Tally — Agent Guide

This document helps AI agents understand the codebase structure, conventions, and design philosophy.

**Keep this file up-to-date.** When you add features, change architecture, or modify conventions, update this document before committing.

## Commands

```bash
cargo build           # Build
cargo test            # Run tests
cargo run             # Refresh transactions from exports/
cargo run -- --tui    # Launch terminal UI
```

## Project Goals

Tally is a personal finance tool for aggregating bank transactions. Key principles:

- **Privacy first** — All data stays local in SQLite, no cloud sync
- **Bank agnostic** — Import scripts adapt to any bank's CSV format
- **Minimal UI** — TUI uses whitespace over borders, context over labels
- **AI-assisted** — Categories and transfers can be suggested by AI, confirmed by user

## Architecture

```
src/
├── main.rs                 # CLI entry point, argument parsing
├── lib.rs                  # Public API exports
├── types.rs                # Core data structures
├── db.rs                   # SQLite schema and initialization
├── store.rs                # TransactionStore: all database operations
├── import.rs               # Import script execution and file discovery
├── search.rs               # Search query parser and fuzzy matcher
├── error.rs                # Error types
└── tui/
    ├── mod.rs              # TUI entry point, event loop, key handling
    ├── app.rs              # Application state, actions, data loading
    └── ui.rs               # Rendering functions for all views

exports/                    # Bank export files (user data, gitignored)
├── {BankName}/
│   ├── import              # Bank-level import script
│   └── {AccountName}/
│       ├── import          # Account-level import script (overrides bank)
│       └── *.csv           # Raw bank exports

tally.db                    # SQLite database (gitignored)
```

## Key Types

### Transaction
```rust
Transaction {
    id: i64,
    bank_id: i64,
    account_id: i64,
    date: NaiveDate,
    description: String,
    amount_cents: i64,      // Negative = debit, positive = credit
    balance_cents: i64,
    hash: String,           // For deduplication
    metadata: HashMap<String, Value>,
    source_file: String,
    import_batch_id: i64,
}
```

### Category & Enrichment
- `Category { id, path, created_at }` — Hierarchical paths like "Food/Groceries"
- `TransactionEnrichment` — Links transaction to category, tracks source (manual/ai/rule) and confirmation status
- `TransactionWithEnrichment` — Transaction + enrichment + resolved category

### Transfer
- `Transfer { from_transaction_id, to_transaction_id, source, confirmed }` — Links two transactions as a transfer
- `TransferWithTransactions` — Transfer + both resolved transactions

## Database Schema

**Core tables:**
- `banks` — `id, name, deleted_at`
- `accounts` — `id, bank_id, name, deleted_at`
- `transactions` — `id, account_id, date, description, amount_cents, balance_cents, hash, metadata, source_file, import_batch_id`

**Enrichment tables:**
- `categories` — `id, path, created_at`
- `transaction_enrichments` — `id, transaction_id, category_id, category_source, category_confirmed, ai_confidence, created_at, updated_at`
- `transfers` — `id, from_transaction_id, to_transaction_id, source, confirmed, created_at`

**Import tracking:**
- `imported_files` — `id, account_id, path, content_hash, imported_at, import_batch_id`
- `import_batches` — `id, started_at, completed_at`

## Design Decisions

### Money as Cents (i64)
All monetary values are integers in cents to avoid floating-point errors.
- $123.45 → `12345`
- -$50.00 → `-5000`

### Deduplication
- Transactions deduplicated by `(account_id, hash)`
- Hash computed from: `date|description|amount_cents|balance_cents`
- Files tracked by content hash to skip re-importing unchanged files

### Soft Deletes
Banks/accounts that disappear from `exports/` are soft-deleted (`deleted_at` timestamp) to preserve historical data.

### No Migrations
Schema changes require deleting `tally.db` and re-importing. This keeps the codebase simple for a personal tool.

## TUI Architecture

### State (app.rs)
- `App` holds all UI state: current tab, selected index, input mode, cached data
- Data is loaded on startup and refreshed after mutations
- `banks` and `accounts` are cached as HashMaps for O(1) name lookups

### Rendering (ui.rs)
- `draw()` is the entry point, dispatches to tab-specific functions
- Each list view has a corresponding details panel at the bottom
- Details panels show contextual info (transfer partner OR category, not both)
- Popups rendered last to overlay content

### Key Handling (mod.rs)
- `InputMode` enum controls which keys are active
- Normal mode: navigation, tab switching, action triggers
- Category mode: text input with fuzzy-matched suggestions
- Transfer modes: candidate selection and confirmation

## TUI Aesthetics

- **Whitespace over borders** — No box borders on tables or panels
- **Context over labels** — Tab names provide context, no redundant headers
- **Color coding:**
  - Red: negative amounts, "from" in transfers
  - Green: positive amounts, "to" in transfers
  - Yellow: categories, pending items
  - Cyan: transfer indicators, confidence scores
  - DarkGray: labels, disabled items

## Common Store Operations

```rust
// Querying
store.query_transactions(&filter) -> Vec<Transaction>
store.get_uncategorized_transactions(limit) -> Vec<Transaction>
store.get_pending_ai_reviews(limit) -> Vec<TransactionWithEnrichment>
store.get_pending_transfer_reviews(limit) -> Vec<Transfer>

// Categories
store.get_or_create_category(path) -> i64
store.set_category(tx_id, cat_id, source, confirmed, confidence)
store.get_transaction_category(tx_id) -> Option<Category>

// Transfers
store.find_matching_transfer_candidates(tx) -> Vec<Transaction>
store.create_transfer(from_id, to_id, source, confirmed)
store.get_transfer_for_transaction(tx_id) -> Option<Transfer>
```

## Import Script Contract

Scripts receive CSV path as argument, output JSON to stdout:

```json
[{
  "date": "2025-01-15",
  "description": "Payment",
  "amount_cents": -15000,
  "balance_cents": 123456,
  "hash": "optional",
  "metadata": {}
}]
```

## Planned Features

### MCP Server
Model Context Protocol server for AI agent integration:
- Transaction categorization suggestions
- Transfer detection
- Rule creation assistance
