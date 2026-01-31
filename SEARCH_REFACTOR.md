# Search Refactor: Split DB Search and Fuzzy Search

## Overview

Two stackable search modes:
- **`/` (DB search)**: Persists, queries database with all filters including regex
- **`~` (Fuzzy refine)**: Temporary, filters loaded results in memory using nucleo

When both active: DB search runs first, fuzzy filters those results. Escape clears fuzzy first, then DB search.

## Entry Methods

**DB Search (`/`):**
- Press `/` in normal mode

**Fuzzy Search (`~`):**
- Press `~` in normal mode (with or without DB search active)
- Type ` ~` (space + tilde) at end of DB search input → transitions to fuzzy mode, removing ` ~` from query

**Exit:**
- Escape clears fuzzy first, then DB search
- Enter confirms current input and returns to normal mode

## Implementation Steps

### Step 1: Add REGEXP UDF to SQLite ✓
- [x] Register `regexp(pattern, text)` function in `src/db.rs`
- [x] Call from `init_db()`

### Step 2: Extend TransactionFilter and query_transactions ✓
- [x] Add `amount_min`, `amount_max`, `description_regex` to `TransactionFilter`
- [x] Add `bank_name_prefix`, `account_name_prefix` for starts-with matching
- [x] Add `category_contains` with LEFT JOIN to enrichments/categories
- [x] Update `query_transactions()` to build SQL for all new filters

### Step 3: Create new search query types ✓
- [x] `DbSearchQuery` with structured filters + text/regex
- [x] `DbTextMatch::Substring` and `DbTextMatch::Regex`
- [x] Fuzzy search is just a pattern string in App
- [x] `to_filter()` converts DbSearchQuery to TransactionFilter

### Step 4: Update App state ✓
- [x] Replace single search with `db_search_input`, `fuzzy_search_input`
- [x] Add `db_search_active`, `fuzzy_search_active` flags
- [x] Update `InputMode` enum (DbSearch, FuzzySearch)

### Step 5: Implement stacked filtering logic ✓
- [x] `reload_from_db()` queries DB with `db_search_query.to_filter()`
- [x] `apply_fuzzy_filter()` filters loaded results with nucleo
- [x] Works on all tabs (transactions, transfers, uncategorized, ai_reviews, transfer_reviews)

### Step 6: Update key handling ✓
- [x] `/` starts DB search
- [x] `~` starts fuzzy search (from normal mode)
- [x] ` ~` at end of DB search input transitions to fuzzy
- [x] Escape clears fuzzy first, then DB search
- [x] Enter confirms current input

### Step 7: Update UI rendering ✓
- [x] Show both search bars when both active (DB above, fuzzy below)
- [x] Cursor in active input
- [x] Color coding: cyan for DB search, yellow for fuzzy

## Filter Behavior

| Filter | Backend | Matching |
|--------|---------|----------|
| `date:` | SQL | Range (supports `..`, `>`, `<`) |
| `amount:` | SQL | Range (supports `..`, `>`, `<`) |
| `bank:` | SQL | Starts-with, case-insensitive |
| `account:` | SQL | Starts-with, case-insensitive |
| `category:` | SQL | Contains, case-insensitive |
| plain text | SQL | LIKE `%pattern%` |
| `/pattern/` | SQL | REGEXP UDF |
| `~` mode | Memory | nucleo fuzzy match |

## Files to Modify

- `src/db.rs` - REGEXP function
- `src/types.rs` - TransactionFilter fields
- `src/store.rs` - query_transactions SQL building
- `src/search.rs` - new query types and parsing
- `src/tui/app.rs` - state, filtering logic
- `src/tui/mod.rs` - key handling
- `src/tui/ui.rs` - rendering

## Progress

Started: Yes
Completed: Yes
