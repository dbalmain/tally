# Performance Refactoring Plan

Prioritized improvements identified from code review. Work through these one at a time.

## Medium Priority

### 2. [x] Improve `query_transactions` parameter building

**Why:** Currently uses `Vec<Box<dyn ToSql>>` which heap-allocates per parameter.

**Current:**
```rust
let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
params_vec.push(Box::new(bank_id));
```

**Target:** Use `rusqlite::params_from_iter` with `Vec<rusqlite::types::Value>` or build query variants with `params![]`.

**Files:** `src/store.rs`

**Effort:** M (1-3h)

---

### 3. [x] Push more filtering into SQL

**Why:** Currently loads 500 transactions then filters in memory. For structured filters (date, amount, bank, account), SQL is more efficient.

**Current:** `apply_search_filter()` does in-memory filtering after loading.

**Target:** Build dynamic SQL WHERE clauses for date/amount/bank/account filters.

**Solution:** Split search into two modes:
- `/` (DB search): All structured filters pushed to SQL via `DbSearchQuery.to_filter()`
- `~` (Fuzzy search): In-memory nucleo fuzzy matching on loaded results
- Both can be active simultaneously (stacked)
- See SEARCH_REFACTOR.md for full implementation details

**Files:** `src/search.rs`, `src/tui/app.rs`, `src/tui/mod.rs`, `src/tui/ui.rs`, `src/store.rs`

**Effort:** M (2-4h)

---

## Code Quality

### 6. [ ] Add `bank_id`/`account_id` filter support for transfers

**Why:** `build_transfer_filter_clause` handles `bank_name_prefix` and `account_name_prefix` but silently ignores `bank_id` and `account_id`, causing inconsistent behavior.

**Target:** Add OR-based filtering for ID fields, matching the pattern used for name prefixes.

**Files:** `src/store.rs`

**Effort:** S (<1h)

---

### 7. [ ] Move `use rusqlite::types::Value` to module level

**Why:** Currently repeated in each function that builds dynamic SQL. Module-level import reduces repetition.

**Files:** `src/store.rs`

**Effort:** S (<30m)

---

## Future Scaling (10k+ transactions)

### 4. [ ] Add SQLite indices

**Why:** Improves query performance for large datasets.

**Target:**
```sql
CREATE INDEX idx_transactions_date ON transactions(date);
CREATE INDEX idx_transactions_account ON transactions(account_id);
CREATE INDEX idx_transactions_amount ON transactions(amount_cents);
```

**Files:** `src/db.rs`

**Effort:** S (<1h)

---

### 5. [ ] Consider FTS5 for text search

**Why:** Full-text search is faster than LIKE for large datasets.

**Target:**
- Store normalized lowercase descriptions
- Use SQLite FTS5 virtual table for description matching

**Files:** `src/db.rs`, `src/store.rs`

**Effort:** L (1-2 days)

---

## Completed

- [x] Remove unnecessary `.clone()` in UI table rendering
- [x] Use `serde_json::from_slice` instead of `from_str`
- [x] Stream file hashing instead of loading entire file
- [x] Use binary encoding for hash integers
- [x] Pre-lowercase exact match patterns at parse time
- [x] Reuse `Vec<char>` buffer in `FuzzyMatcher`
- [x] Fix N+1 query in `list_transfers_with_transactions` (single JOIN)
- [x] Safe datetime parsing (no `.unwrap()`)
- [x] Add caches (`tx_by_id`, `category_by_tx_id`, `transfer_by_tx_id`)
- [x] Fix duplicate `list_banks()` call
- [x] Change filtered lists to indices instead of cloning
