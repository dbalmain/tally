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
cargo run             # Launch terminal UI
cargo run -- pull     # Refresh transactions from exports/
cargo run -- tui      # Launch terminal UI (--tui also accepted)
cargo run -- classify # Suggest categories locally (temporal history + TF-IDF/SVM)
cargo run -- --vault PATH  # Use PATH as the vault root (or set FM_VAULT)
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
| Scrollable table component / inline detail panel / column geometry | `src/tui/table.rs` |
| Normal-mode key dispatch / footer hints / `?` popover | `src/tui/keymap.rs` (`normal_binds` is the single source of truth) |
| Modal-mode key dispatch | `src/tui/mod.rs` (one match per modal `InputMode`; update curated hints in `src/tui/keymap.rs` too) |
| App state, actions, data loading, caches | `src/tui/app/mod.rs` |
| Tab definitions / per-tab data & dispatch | `src/tui/app/tabs.rs` |
| DB-search / fuzzy-search behaviour in the app | `src/tui/app/search.rs`; Categories search is applied in memory by `src/tui/app/tabs.rs` (`/` path boundary-prefix, `~` path fuzzy) |
| Category actions (assign, rename, merge, AI review) | `src/tui/app/categories.rs` |
| Filter actions (create, rename, categorise, override/review/delete) | `src/tui/app/filters.rs` |
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
├── classify/               # Pure temporal + TF-IDF/SVM classification, similarity index, adapter
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
    ├── keymap.rs           # Normal-mode key table + footer/help hint text
    ├── ui.rs               # Rendering: layout, tables, details, popups
    ├── table.rs            # Domain-agnostic scrollable table + inline detail panel geometry
    ├── search_bar.rs       # Search bar widget (context-aware keys, autocomplete)
    ├── filtered_list.rs    # FilteredList<T>: items + fuzzy-filtered view
    └── app/
        ├── mod.rs          # App struct, construction, caches, navigation, bulk-apply state
        ├── tabs.rs         # Tab/TodoSubTab enums + TabLists (per-tab dispatch)
        ├── search.rs       # TabSearchState + search/autocomplete actions
        ├── categories.rs   # Category popup, AI review, rename/merge
        ├── filters.rs      # Saved-search filter management
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
- `Filter { name, query, category_id, override_mode, review_required }` — Saved search that can apply rule-sourced categories via `store.apply_filters()`

### Transfer
- `Transfer { from_transaction_id, to_transaction_id, source, confirmed, ai_confidence }` — Links two transactions as a transfer
- `TransferWithTransactions` — Transfer + both resolved transactions

## Database Schema

**Core tables:**
- `banks` — `id, name, deleted_at`
- `accounts` — `id, bank_id, name, deleted_at`
- `transactions` — `id, account_id, date, description, amount_cents, balance_cents, hash, metadata, source_file, import_batch_id`

**Enrichment tables:**
- `categories` — `id, path, created_at`
- `transaction_enrichments` — `id, transaction_id, category_id, category_source, category_confirmed, ai_confidence, created_at, updated_at`
- `transfers` — `id, from_transaction_id, to_transaction_id, source, confirmed, ai_confidence, created_at`
- `filters` — `id, name, query, category_id, override_mode, review_required, position, created_at`

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

### AI Classification Pipeline

`tally classify` first detects likely transfers among uncategorised,
not-yet-transferred transactions: same-day opposite amounts in different
accounts, paired greedily with history-aware confidence. Detected transfers use
source `auto`, remain unconfirmed, and are excluded from category suggestions.
It then trains only from confirmed category enrichments, first reusing the most
recent prior same-biller category and preferring an exact amount; novel billers
fall back to word/character TF-IDF plus a one-vs-rest linear SVM. The pure
`train`/`predict` and transfer-detection pipeline lives under `src/classify/`,
with storage isolated in `adapter.rs`. Category suggestions use source `ai`,
remain unconfirmed, and never replace an existing enrichment. No configuration
or external service is required.

When the category popup manually assigns a category, the TUI can offer to apply
the same category to other unconfirmed transactions whose normalised
descriptions are strong TF-IDF cosine matches. The precomputed pure index lives
in `src/classify/similarity.rs`; the cutoff is `SIMILARITY_THRESHOLD`.

### Transfer / category are mutually exclusive

A transaction is either part of a transfer or categorised, never both. The
invariant is enforced in `store.create_transfer`, which deletes any enrichment
on both endpoints (a no-op for the uncategorised rows AI detection picks). The
TUI guards the inverse: categorising a transfer (`c`) prompts to unlink it
first, and marking a transfer (`t`) whose chosen endpoint is already linked
prompts to break the existing transfer(s). Both prompts run through the generic
`InputMode::Confirm` / `ConfirmAction` flow (`App::confirm_proceed`). Transfer
candidate search (`store.transfer_candidates`) therefore no longer hides
already-linked transactions — it offers them and the caller confirms.

### Saved filters

Filters are saved searches. When a filter has a category, `store.apply_filters()`
re-derives rule-sourced categories from the saved filter set.
`store.preview_filters()` is the dry-run: it runs the real apply body inside a
SQLite savepoint and rolls back, returning the transactions that *would* be
(re)categorised without persisting anything.
On Transactions, `Ctrl-S` saves the active DB search as a new filter and opens
the filter edit screen for it. Applying filter categories (`a` on the Filters
tab, `Ctrl-A` on the Filter Edit screen) opens a scrollable confirmation modal
listing the affected transactions (date/description/amount) before applying.
Deleting a filter (`d`/`Delete`) and
leaving the edit screen with unsaved query edits (`Esc`) both prompt for
confirmation first.

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
codebase simple for a personal tool. Views and additive cache tables are
exempt: views are recreated on every open, and caches use
`CREATE TABLE IF NOT EXISTS` because they do not change core stored data.

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
- **Overlay modals use shared chrome** — use `src/tui/modal.rs` `Modal`; modal
  keybind hints render inside the modal and hide the global hint bar.
- **Tables use `ScrollTable`** (`src/tui/table.rs`) — it owns scroll-offset
  math, inline detail placement, and column geometry; per-view closures own row
  content, styling, and optional detail rendering.
- **Normal-mode keys live in `keymap.rs`** — `normal_binds(app)` drives
  dispatch, the bottom key-hint bar, and the `?` keybind popover. The hint bar
  starts visible each launch; `Alt-?` toggles it for the session.

### Aesthetics

- **Whitespace over borders** — No box borders on tables or panels
- **Context over labels** — Tab names provide context, no redundant headers
- **Row-level styling** — Use `Row::style()` for backgrounds, not per-cell `.bg()`
- **Color coding:** Red = negative amounts / transfer "from"; Green = positive
  amounts / transfer "to"; Yellow = categories, pending items; Cyan = transfer
  indicators, confidence scores; DarkGray = labels, disabled items

## Performance

`refresh()` discovers pull scripts and runs them in parallel before opening the
DB write transaction, capped at six concurrent pulls. CSV import and all DB
writes remain serial inside the transaction, preserving deduplication,
soft-deletes, imported-file tracking, and batch accounting.

`tally tui` opens immediately from the existing database, starts `refresh()` on
a second store connection, and reloads the visible lists when that background
refresh commits. File-backed stores enable SQLite WAL journal mode plus a
5-second busy timeout so the foreground TUI can keep reading while the
background connection writes; WAL sidecar files (`-wal`/`-shm`) are expected.

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
3. `src/tui/ui.rs` — add a `draw_…` function (use `ScrollTable`) and dispatch
   to it from `draw()`.
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

1. Implement the action as an `App` method in the matching feature file
   (`app/categories.rs`, `app/transfers.rs`, `app/search.rs`, or `app/mod.rs`).
2. For Normal mode, add one `Bind` row to `normal_binds` in `src/tui/keymap.rs`
   (plus an `Act` variant and `run_normal` arm if needed). Footer and popover
   text come from that same row.
3. For a modal mode, update the matching `InputMode` arm in `src/tui/mod.rs`
   and its curated `footer_hints` / `help_lines` arm in `src/tui/keymap.rs`.
   Text-editing keys are shared via `text_edit_request`; only add mode-specific
   keys.
4. Update the key-binding tables below.

## TUI Key Bindings

These tables document intent. Normal mode is implemented by `src/tui/keymap.rs`;
modal handlers live in `src/tui/mod.rs` with curated hints in `keymap.rs`.

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
| `Ctrl-S` | Save current Transactions DB search as a filter |
| `n` | Create filter (Filters tab) |
| `a` | Apply filter categories (Filters tab) — opens a scrollable confirmation modal listing the affected transactions (date/description/amount) before applying. On the Filter Edit screen this is `Ctrl-A` |
| `c` | Set category on transaction (including Todo → AI Review), or set/clear filter category (Filters tab); categorising a transfer prompts to unlink it first |
| `e` / `r` | Rename category (`e`, Categories tab), or rename filter (`r`, Filters tab) |
| `o` | Cycle filter override mode (Filters tab: `new` → `+ai` → `all`) |
| `v` | Toggle filter review requirement (Filters tab) |
| `t` | Mark as transfer (including Todo → AI Review); if a chosen endpoint is already linked, prompts to break the existing transfer |
| `d` / `Delete` | Delete transfer (Transfers tab), or delete filter (Filters tab; prompts to confirm) |
| `Delete` | Transactions tab: unlink transfer, else remove category. AI Review: remove category. Transfer Review: unlink transfer |
| `M` | Toggle source + metadata lines in the transaction detail (Transactions tab, Todo → Uncategorised) |
| `Enter` | Confirm (AI review, transfer review), or open filter edit (Filters tab) |
| `Esc` | Clear active search (fuzzy first, then DB) |
| `?` | Show keybind popover |
| `Alt-?` | Toggle bottom key-hint bar |

### Search Modes (DB and Fuzzy)
| Key | Action |
|-----|--------|
| `Esc` | Clear search and exit |
| `Enter` | Confirm search |
| `Ctrl-S` | Save current Transactions DB search as a filter |
| `↑` / `↓` | Navigate results |
| `Tab` | Switch tabs (keeps search active) |
| Standard text editing | Left/Right, Ctrl+Left/Right, Home/End, Backspace, Delete |
| `Alt-?` | Toggle bottom key-hint bar |

### Filter Edit
Opened with `Enter` on a Filters-tab row. Full-screen DB-query editor with a
live read-only transaction preview. The heading shows the filter's category,
override mode, and review state so the `Ctrl-O` / `Ctrl-V` toggles give
feedback.

| Key | Action |
|-----|--------|
| `Enter` | When the autocomplete popup is open, accept the suggestion (like `Tab`); otherwise save the query, reapply filters, and return to the Filters table |
| `Ctrl-E` | Rename filter |
| `Ctrl-C` | Set or clear filter category |
| `Ctrl-O` | Cycle filter override mode (only when a category is set) |
| `Ctrl-V` | Toggle filter review requirement (only when a category is set) |
| `Ctrl-A` | Apply filter categories — opens a confirmation modal listing the affected transactions before applying |
| `Tab` | Accept autocomplete suggestion when the popup is open |
| `↑` / `↓` | Scroll preview; search cursor stays in the bar (no hint shown) |
| `Esc` | Return to the Filters table; prompts to confirm if the query has unsaved edits |

### Category Popup
| Key | Action |
|-----|--------|
| `Esc` | Cancel |
| `Enter` | Confirm selection |
| `↑` / `↓` | Navigate suggestions |
| Type | Filter categories |
| `Alt-?` | Toggle bottom key-hint bar |

### Bulk Apply Popup
| Key | Action |
|-----|--------|
| `Space` | Toggle selected row |
| `a` | Toggle all rows |
| `Enter` | Apply to selected rows |
| `Esc` | Cancel |
| `↑` / `↓` | Navigate rows |
| `Alt-?` | Toggle bottom key-hint bar |

### Confirmation Popups
| Key | Action |
|-----|--------|
| `y` / `Enter` | Confirm |
| `n` / `Esc` | Cancel |
| `?` | Show keybind popover |
| `Alt-?` | Toggle bottom key-hint bar |

### Transfer Popups
| Key | Action |
|-----|--------|
| `↑` / `↓` | Navigate candidates |
| `T` / `Enter` | Link selected transfer candidate |
| `t` | Re-search from the selected transaction |
| `Esc` | Cancel or dismiss |
| `?` | Show keybind popover |
| `Alt-?` | Toggle bottom key-hint bar |

## Search Syntax (summary)

Full reference: the `src/search/mod.rs` doc comment (canonical).

- **DB search (`/`)** pushes filters to SQL: `date:2024-01..2024-06`,
  `amount:>100` (precision-aware for bare values), `account:ING/Orange`
  (Bank/Account prefixes, `|` for OR), `category:Food|Transport`. Bare words
  are FTS5 full-text search (`coffee OR tea`, `"exact phrase"`, `coff*`);
  `/pattern/i` is regex. End with ` ~` to switch to fuzzy mode keeping the
  DB filters.
- **Fuzzy search (`~`)** is in-memory nucleo scoring over the loaded rows.
- On the Categories tab, **DB search (`/`)** is not SQL-backed: it is an
  in-memory, case-insensitive boundary-prefix filter over the category path
  (boundaries are the start of the path or positions after non-alphanumeric
  characters). **Fuzzy search (`~`)** is fuzzy over the path.
- On the Filters tab, **DB search (`/`)** and **fuzzy search (`~`)** are
  in-memory fuzzy matches over the filter name and saved query text.

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
store.get_unconfirmed_transactions(&query, limit) -> Vec<Transaction>
store.get_pending_ai_reviews(&query, limit) -> Vec<TransactionWithEnrichment>
store.get_pending_transfer_reviews(&query, limit) -> Vec<Transfer>
store.list_transfers_with_transactions(confirmed_only, &query, limit) -> Vec<TransferWithTransactions>
store.get_confirmed_examples() -> Vec<ConfirmedCategoryExample>
store.get_confirmed_transfer_examples() -> Vec<ConfirmedTransferExample>

// Categories
store.get_or_create_category(path) -> i64
store.set_category(tx_id, cat_id, source, confirmed, confidence)
store.get_transaction_category(tx_id) -> Option<Category>

// Transfers
store.find_matching_transfer_candidates(tx) -> Vec<Transaction>
store.create_transfer(from_id, to_id, source, confirmed, confidence)
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
