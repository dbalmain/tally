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

### Step 3: Create new search query types
- [ ] `DbSearchQuery` with structured filters + text/regex
- [ ] `DbTextMatch::Substring` and `DbTextMatch::Regex`
- [ ] `FuzzySearchQuery` (just pattern string)
- [ ] Parsing for both

### Step 4: Update App state
- [ ] Replace single search with `db_search_input`, `fuzzy_search_input`
- [ ] Add `db_search_active`, `fuzzy_search_active` flags
- [ ] Update `InputMode` enum

### Step 5: Implement stacked filtering logic
- [ ] `apply_filters()` reloads from DB when db_search changes
- [ ] Fuzzy filter applies on top of loaded results
- [ ] Works on all tabs (transactions, transfers, uncategorized, ai_reviews, transfer_reviews)

### Step 6: Update key handling
- [ ] `/` starts DB search
- [ ] `~` starts fuzzy search (from normal mode)
- [ ] ` ~` at end of DB search input transitions to fuzzy
- [ ] Escape clears fuzzy first, then DB search
- [ ] Enter confirms current input

### Step 7: Update UI rendering
- [ ] Show both search bars when both active (DB above, fuzzy below)
- [ ] Cursor in active input
- [ ] Color coding: cyan for DB search, yellow for fuzzy

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
Current step: 3
