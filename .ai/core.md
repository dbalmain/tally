# Tally — Agent Guide

This is the canonical project guide for all AI agents. The per-agent files
(`CLAUDE.md`, `AGENTS.md`) are thin stubs that point here.

**Keep this file up-to-date.** When you add features, change architecture, or
modify conventions, update this document before committing.

**This file is a router, not a mirror.** Detailed reference docs live in module
doc comments (single source of truth); this file tells you where to look and
what conventions to follow. When you find a discrepancy, the code is right —
fix this file.

## Commands

```bash
cargo build           # Build
cargo test            # Run tests
cargo run             # Refresh transactions from exports/
cargo run -- tui      # Launch terminal UI (--tui also accepted)
cargo run -- --collection PATH  # Use PATH as the collection root (or set FM_COLLECTION)
```

## Project Goals

Tally is a personal finance tool for aggregating bank transactions. Key principles:

- **Privacy first** — All data stays local in SQLite, no cloud sync
- **Bank agnostic** — Import scripts adapt to any bank's CSV format
- **Minimal UI** — TUI uses whitespace over borders, context over labels
- **AI-assisted** — Categories and transfers can be suggested by AI, confirmed by user

## Where to Make Changes

| You want to change… | Look in |
|---|---|
| Colors, layout, table columns, details panels, popups | `src/tui/ui.rs` |
| What a key does | `src/tui/mod.rs` (one match per `InputMode`; text editing shared via `text_edit_request`) |
| App state, actions, data loading, caches | `src/tui/app/mod.rs` |
| Tab definitions / per-tab data & dispatch | `src/tui/app/tabs.rs` |
| DB-search / fuzzy-search behaviour in the app | `src/tui/app/search.rs` |
| Category actions (assign, rename, merge, AI review) | `src/tui/app/categories.rs` |
| Transfer actions (mark, confirm, delete) | `src/tui/app/transfers.rs` |
| Search-bar widget (rendering, cursor-context keys, autocomplete) | `src/tui/search_bar.rs` |
| SQL queries / store methods | `src/store.rs` |
| Schema, FTS index, `transactions_view` | `src/db.rs` |
| Search syntax, parsing, SQL rendering | `src/search/` (syntax reference: `src/search/mod.rs` doc comment) |
| A single filter's behaviour (date, amount, account, category) | `src/search/filters/<name>.rs` |
| Import script discovery/execution | `src/import.rs` |
| Core data structures | `src/types.rs` |

## Architecture

```
src/
├── main.rs                 # CLI entry point, argument parsing
├── lib.rs                  # Public API exports
├── types.rs                # Core data structures
├── db.rs                   # SQLite schema, transactions_view, FTS index
├── store.rs                # TransactionStore: all database operations
├── import.rs               # Import script execution and file discovery
├── logging.rs              # File logger (TALLY_LOG=debug, ~/.local/share/tally/)
├── error.rs                # Error types
├── search/                 # Search query system
│   ├── mod.rs              # Module entry + CANONICAL QUERY SYNTAX REFERENCE
│   ├── tokenize.rs         # Raw token splitter (filter/regex/fts/whitespace)
│   ├── parse.rs            # Token → ParsedQuery + SearchConfig (filter registry)
│   ├── query.rs            # ParsedQuery / QueryPart / Span types
│   ├── render.rs           # SqlContext + ParsedQuery::render → WHERE+params
│   ├── filter.rs           # Filter trait
│   ├── filters/            # Built-in filters (date, amount, account, category)
│   ├── context.rs          # CursorContext for key handling
│   └── fuzzy.rs            # Nucleo-based fuzzy matcher
└── tui/
    ├── mod.rs              # TUI entry point, event loop, key dispatch
    ├── ui.rs               # Rendering: layout, tables, details, popups
    ├── search_bar.rs       # Search bar widget (context-aware keys, autocomplete)
    ├── filtered_list.rs    # FilteredList<T>: items + fuzzy-filtered view
    └── app/
        ├── mod.rs          # App struct, construction, caches, navigation
        ├── tabs.rs         # Tab/TodoSubTab enums + TabLists (per-tab dispatch)
        ├── search.rs       # TabSearchState + search/autocomplete actions
        ├── categories.rs   # Category popup, AI review, rename/merge
        └── transfers.rs    # Transfer marking, confirmation, deletion

tools/                      # Generic, checked-in helper tools (no user data)
└── pocketsmith-pull        # PocketSmith → Tally puller (see Import Sources)

exports/                    # Bank export files (user data, gitignored)
├── {BankName}/
│   ├── import              # Bank-level import script
│   └── {AccountName}/
│       ├── import          # Account-level import script (overrides bank)
│       ├── *.csv           # Raw bank exports (CSV-drop import)
│       ├── pull            # Account-level pull script (API-fetch import)
│       └── pull.log        # Per-account incremental-pull log (generated)

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

**Read-side view:**
- `transactions_view` — a transaction joined to its account and bank
  (`bank_id, bank_name, account_name, account_deleted_at` extra columns). All
  store read queries go through this view so the join exists in one place.
  It's dropped and recreated on every open, so changing it in `db.rs` needs no
  migration.

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
Schema changes require deleting `tally.db` and re-importing. This keeps the
codebase simple for a personal tool. (Views are exempt: they're recreated on
every open.)

## TUI Conventions

These are the patterns every TUI change should follow — they exist so error
handling and data flow stay uniform:

- **Mutations go through `App::try_mutation`** — it surfaces DB errors via the
  error popup and returns `bool` so callers can gate `refresh_data()` on
  success. Never call a mutating store method directly and ignore the result.
- **Mid-flight loads go through `App::load_or_show`** — same error surfacing,
  returns `T::default()` on failure.
- **After any mutation, call `app.refresh_data()`** — reloads the current
  tab, rebuilds caches and category counts.
- **Lists are `FilteredList<T>`** — the DB query result is the item set; the
  fuzzy filter is a view over it (`refilter`/`show_all`). Indices the user
  sees are *visible* indices.
- **Per-tab anything goes through `TabLists`** (`app/tabs.rs`) — don't add
  `match app.current_tab` to other files; add a method to `TabLists` instead.
- **Rendering never queries the DB** — `ui.rs` reads `App` state and the
  caches (`get_cached_transaction/category/transfer`).
- **Tables use `draw_scrolled_table`** (`ui.rs`) — it owns scroll-offset math;
  the per-view closure owns row content and styling.

### Aesthetics

- **Whitespace over borders** — No box borders on tables or panels
- **Context over labels** — Tab names provide context, no redundant headers
- **Row-level styling** — Use `Row::style()` for backgrounds, not per-cell `.bg()`
- **Color coding:** Red = negative amounts / transfer "from"; Green = positive
  amounts / transfer "to"; Yellow = categories, pending items; Cyan = transfer
  indicators, confidence scores; DarkGray = labels, disabled items

## Recipes

### Adding a New Search Filter

1. Implement `Filter` in a new file under `src/search/filters/`. `parse(value)`
   returns SQL with placeholders like `{date}`, `{bank_name}`, `{category_path}`
   — never bare column references.
2. Declare placeholder dependencies in `requires()` so the renderer knows which
   contexts can apply the filter.
3. Register in `src/search/filters/mod.rs` and in `SearchConfig::standard`
   (`src/search/parse.rs`) — the single registration point; every search bar
   picks it up from there.
4. If the filter needs a column not yet in the standard contexts, add it to
   `transactions_view` in `src/db.rs` and to `transaction_ctx()` /
   `transfer_side_ctx()` in `src/store.rs`. If the placeholder requires a
   JOIN, extend `transaction_joins()` to splice it in when
   `parsed.uses_placeholder("your_placeholder")`.
5. Document the syntax in the `src/search/mod.rs` doc comment.

Store query methods don't change; the search bar UI is filter-agnostic.

### Adding a New Tab (or Todo Subtab)

1. `src/tui/app/tabs.rs` — add the enum variant (+ `all()`/`title()`), add a
   `FilteredList` field to `TabLists`, and extend each `TabLists` method:
   `load`, `reload`, `len`, `apply_fuzzy`, and (only if the tab's rows are
   plain transactions) `transaction_at` / `position_of_tx`.
2. `src/tui/app/search.rs` — decide the tab's filters in
   `build_search_config`.
3. `src/tui/ui.rs` — add a `draw_…` function (use `draw_scrolled_table`) and
   dispatch to it from `draw()`.
4. If new data feeds the caches, extend `rebuild_tx_caches` in
   `src/tui/app/mod.rs`.
5. Update the key-binding/tab docs in this file.

### Adding a Column to Transactions

1. `src/db.rs` — add the column to the `transactions` table and to
   `transactions_view` (keep the view's leading columns in
   `parse_transaction_at_offset` order).
2. `src/types.rs` — add the field to `Transaction`.
3. `src/store.rs` — add the column name to `tx_cols()` and parse it in
   `parse_transaction_at_offset` (order matters); extend `insert_transaction`.
4. Delete `tally.db` and re-import (no migrations).

### Adding a Key Binding

1. `src/tui/mod.rs` — add to the right `InputMode` arm. Text-editing keys are
   shared via `text_edit_request`; only add mode-specific keys.
2. Implement the action as an `App` method in the matching feature file
   (`app/categories.rs`, `app/transfers.rs`, `app/search.rs`, or `app/mod.rs`).
3. Update the key-binding tables below.

## TUI Key Bindings

These tables document intent; `src/tui/mod.rs` is the implementation. Update
both together.

### Global (Normal Mode)
| Key | Action |
|-----|--------|
| `q` | Quit |
| `j` / `↓` | Next item |
| `k` / `↑` | Previous item |
| `Tab` / `Shift+Tab` | Next/previous tab |
| `[` / `]` | Previous/next subtab (Todo) |
| `/` | Start DB search |
| `~` | Start fuzzy search |
| `c` | Set category on transaction |
| `e` | Rename category (Categories tab) |
| `t` | Mark as transfer |
| `d` | Delete transfer |
| `Enter` | Confirm (AI review, transfer review) |

### Search Modes (DB and Fuzzy)
| Key | Action |
|-----|--------|
| `Esc` | Clear search and exit |
| `Enter` | Confirm search |
| `↑` / `↓` | Navigate results |
| `Tab` | Switch tabs (keeps search active) |
| Standard text editing | Left/Right, Ctrl+Left/Right, Home/End, Backspace, Delete |

### Category Popup
| Key | Action |
|-----|--------|
| `Esc` | Cancel |
| `Enter` | Confirm selection |
| `↑` / `↓` | Navigate suggestions |
| Type | Filter categories |

### Confirmation Popups
| Key | Action |
|-----|--------|
| `y` / `Enter` | Confirm |
| `n` / `Esc` | Cancel |

## Search Syntax (summary)

Full reference: the `src/search/mod.rs` doc comment (canonical).

- **DB search (`/`)** pushes filters to SQL: `date:2024-01..2024-06`,
  `amount:>100` (precision-aware for bare values), `account:ING/Orange`
  (Bank/Account prefixes, `|` for OR), `category:Food|Transport`. Bare words
  are FTS5 full-text search (`coffee OR tea`, `"exact phrase"`, `coff*`);
  `/pattern/i` is regex. End with ` ~` to switch to fuzzy mode keeping the
  DB filters.
- **Fuzzy search (`~`)** is in-memory nucleo scoring over the loaded rows.

## Common Store Operations

All read methods take a `ParsedQuery` plus an optional `limit`. Build a query
with `search::parse(&config, input, cursor).0`, or pass `ParsedQuery::empty()`
for "no filters". Read queries run against `transactions_view`; the column
lists come from `tx_cols(alias)` / `enrichment_cols(alias)` / `TRANSFER_COLS`,
whose order must match the `parse_*_at_offset` row parsers.

```rust
// Querying (all take &ParsedQuery + Option<usize> limit)
store.query_transactions(&query, limit) -> Vec<Transaction>
store.get_uncategorised_transactions(&query, limit) -> Vec<Transaction>
store.get_pending_ai_reviews(&query, limit) -> Vec<TransactionWithEnrichment>
store.get_pending_transfer_reviews(&query, limit) -> Vec<Transfer>
store.list_transfers_with_transactions(confirmed_only, &query, limit) -> Vec<TransferWithTransactions>

// Categories
store.get_or_create_category(path) -> i64
store.set_category(tx_id, cat_id, source, confirmed, confidence)
store.get_transaction_category(tx_id) -> Option<Category>

// Transfers
store.find_matching_transfer_candidates(tx) -> Vec<Transaction>
store.create_transfer(from_id, to_id, source, confirmed)
store.get_transfer_for_transaction(tx_id) -> Option<Transfer>
```

## Import Sources

An account folder under `exports/` is fed by either (or both) of two
mechanisms, both discovered and run by `refresh()` in `src/store.rs`
(`import_account_transactions`). Both emit the same JSON; dedup is by
`(account_id, hash)`, so re-runs are idempotent.

### CSV-drop (`import` script)

`import` receives a CSV path as its argument and outputs a JSON array on
stdout. Each CSV is content-hashed and skipped once imported
(`imported_files`). Account-level scripts override bank-level.

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

### API-pull (`pull` script)

`pull` takes **no argument** and outputs the same JSON array on stdout. It runs
with the account folder as its working directory, so it owns its own
incremental state. There is no file-hash skip — overlap is expected and deduped
by `hash`, so pulls should set a stable `hash` (e.g. the source row's id).
Account-level overrides bank-level (`find_pull_script` / `run_pull_script` in
`src/import.rs`).

**PocketSmith** is wired up via `tools/pocketsmith-pull` (generic, checked-in,
no account data — safe to publish; needs `POCKETSMITH_KEY`):

- `tools/pocketsmith-pull sync` — lists your accounts and creates
  `exports/<Bank>/<Account>/` folders, each with a generated `pull` shim that
  calls `pocketsmith-pull account <id>`. Folders can be freely renamed
  afterwards: the shim keys off the account id, not the folder name, and
  re-running `sync` skips any account whose id already has a shim anywhere
  under `exports/` (so renames survive a re-sync).
- `pull account <id>` — fetches transactions, emits Tally JSON, and appends a
  `<timestamp>\t<from>\t<to>\t<count>` line to `pull.log`. The next pull reads
  the last `to` date and re-fetches from `to − 14 days` (overlap for
  out-of-order rows). First run (no log) fetches full history.
- PocketSmith's category is parked in `metadata.pocketsmith_category` as an
  autocategorisation hint — it is **not** applied to Tally's category system.

## Planned Features

### MCP Server
Model Context Protocol server for AI agent integration:
- Transaction categorization suggestions
- Transfer detection
- Rule creation assistance
