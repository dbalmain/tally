# Search Refactoring Plan

## Overview

Refactor search into a clean, extensible system with:
- Simple tokenization (filters, regex, FTS — no quoted strings or escapes)
- Trait-based pluggable filters that return SQL directly
- Context-aware key handling based on cursor position
- A pre-joined view for all search queries

## Design Principles

1. **Filters have no spaces** — `name:value` where value contains no whitespace
2. **Filters return SQL** — No intermediate `TransactionFilter` type
3. **One multiplicity** — Use `|` (OR) and `&` (AND) within filter values
4. **Context-aware input** — Keys behave differently based on cursor context
5. **Validity feedback** — Visual indication of filter validity

## Tokenization

Simple rules for splitting input:

```
Input: "date:2024 account:ING/Orange /coffee.*/i remaining text"

Tokens:
  - Filter: "date:2024"
  - Filter: "account:ING/Orange"  
  - Regex: "/coffee.*/i"
  - FTS: "remaining text"
```

**Rules:**
- **Filter:** `<name>:<value>` where value has no whitespace
- **Regex:** `/` at word boundary, then content (may include spaces, `\/` for literal slash), then unescaped `/`, then flags until whitespace
- **FTS:** Everything else (whitespace-separated words, quotes for phrases, FTS5 syntax)

No quoted strings or backslash escapes in filter values. Account format `Bank/Account` is fine since `/` only starts regex at word boundary.

## Cursor Context

The search bar tracks which token the cursor is in:

```rust
pub enum CursorContext {
    Filter { name: &'static str, offset: usize },
    Regex { offset: usize },
    Fts { offset: usize },
    Whitespace,
}
```

Key handlers use this to decide behavior:
- `:` in FTS → check if previous word is shortcut, expand/jump
- `|` in dropdown filter → trigger completion popup for next value
- `/` at word boundary in FTS → start regex with `//` cursor between

## Context-Aware Key Handling

### `:` in FTS context
When user types `:` and cursor is in FTS:
1. Look at word before cursor (e.g., `d` in `coffee d:`)
2. Check if it's a filter shortcut or name
3. If matching filter exists in query → delete typed text, jump cursor to that filter
4. If no existing filter → expand shortcut (e.g., `d:` → `date:`), move before FTS

### `|` in dropdown filter context
When user types `|` in a filter that supports completions:
1. Insert the `|`
2. Trigger completion popup for the new segment

### `/` in FTS context
When user types `/` at word boundary:
1. Insert `//` 
2. Place cursor between the slashes

## Filter Trait

```rust
pub trait Filter: Send + Sync {
    /// Canonical name used in search syntax (e.g., "date" for `date:2024`)
    fn name(&self) -> &'static str;
    
    /// Optional shortcut alias (e.g., "d" for `d:2024`)
    fn alias(&self) -> Option<&'static str> { None }
    
    /// Parse the value and return SQL if valid
    fn parse(&self, value: &str) -> FilterResult;
    
    /// Provide completions for dropdown-style filters
    /// Called when cursor is in this filter's value
    /// 
    /// - `value`: full filter value (e.g., "income/sal|income/sales")
    /// - `cursor`: cursor position within the value
    /// 
    /// Returns `Some((suggestions, anchor_offset))` where:
    /// - `suggestions`: list of completion options
    /// - `anchor_offset`: offset within value where popup should anchor (start of current segment)
    /// 
    /// Returns `None` if no completions available (e.g., range filters)
    fn completions(&self, value: &str, cursor: usize) -> Option<(Vec<String>, usize)> {
        None // Default: no completions (range filters)
    }
}


pub enum FilterResult {
    /// Valid filter, here's the SQL
    Valid { sql: String, params: Vec<rusqlite::types::Value> },
    /// Invalid value, here's why
    Invalid(String),
    /// Empty/incomplete value, ignore for now
    Empty,
}
```

### Caching Notes

**Filters handle their own caching internally.** Each filter knows its invalidation semantics:

- `DateFilter`, `AmountFilter`: Pure and simple — caching optional, probably not needed
- `AccountFilter`, `CategoryFilter`: `parse()` is pure (cacheable), `completions()` depends on external state (not cacheable)
- `NamedFilter` (future): Caches expanded SQL, invalidates when saved filters are updated

Filters that need caching maintain an internal `HashMap<String, FilterResult>` and expose an `invalidate()` method if needed. The search bar just calls `parse()` each time — it doesn't manage caching.

```rust
// Example: NamedFilter with internal cache
pub struct NamedFilter {
    saved_filters: HashMap<String, String>,
    cache: RefCell<HashMap<String, FilterResult>>,
}

impl NamedFilter {
    pub fn invalidate(&self) {
        self.cache.borrow_mut().clear();
    }
    
    pub fn update_saved_filter(&mut self, name: &str, query: &str) {
        self.saved_filters.insert(name.to_string(), query.to_string());
        self.invalidate();
    }
}
```

## Filter Implementations

### DateFilter

```rust
pub struct DateFilter;

impl Filter for DateFilter {
    fn name(&self) -> &'static str { "date" }
    fn alias(&self) -> Option<&'static str> { Some("d") }
    
    fn parse(&self, value: &str) -> FilterResult {
        // Supports:
        //   2024        → year range
        //   2024-01     → month range  
        //   2024-01-15  → exact date
        //   2024..2025  → explicit range
        //   ..2024      → up to end of 2024
        //   2024..      → from start of 2024
        
        // Returns: WHERE date >= ? AND date <= ?
    }
}
```

### AmountFilter

```rust
pub struct AmountFilter;

impl Filter for AmountFilter {
    fn name(&self) -> &'static str { "amount" }
    fn alias(&self) -> Option<&'static str> { Some("am") }
    
    fn parse(&self, value: &str) -> FilterResult {
        // Supports:
        //   100        → exactly $100 (10000 cents)
        //   100..500   → range
        //   ..100      → up to $100
        //   100..      → $100 and above
        
        // Returns: WHERE amount_cents >= ? AND amount_cents <= ?
    }
}
```

### AccountFilter

```rust
pub struct AccountFilter {
    // Populated from App.banks and App.accounts
    pub options: Vec<String>,  // "Bank/Account" format
}

impl Filter for AccountFilter {
    fn name(&self) -> &'static str { "account" }
    fn alias(&self) -> Option<&'static str> { Some("a") }
    
    fn parse(&self, value: &str) -> FilterResult {
        // Supports:
        //   ING           → bank prefix
        //   ING/          → all accounts in bank
        //   ING/Orange    → bank + account prefix
        //   /Savings      → any bank, account prefix
        //   ING|NAB       → multiple banks (OR)
        
        // Returns: WHERE (bank_name LIKE ? AND account_name LIKE ?) OR ...
    }
    
    fn completions(&self, value: &str, cursor: usize) -> Option<(Vec<String>, usize)> {
        // 1. Split value by |, find segment containing cursor
        // 2. Determine anchor_offset (start of that segment)
        // 3. Use segment text as typed prefix for fuzzy matching
        // 4. Exclude other segments from results
        // 5. Return Some((suggestions, anchor_offset)) or None if empty
    }
}
```

### CategoryFilter

```rust
pub struct CategoryFilter {
    pub options: Vec<String>,  // Category paths
}

impl Filter for CategoryFilter {
    fn name(&self) -> &'static str { "category" }
    fn alias(&self) -> Option<&'static str> { Some("c") }
    
    fn parse(&self, value: &str) -> FilterResult {
        // Supports:
        //   Food              → contains "Food"
        //   Food/Groceries    → contains "Food/Groceries"
        //   Food|Transport    → multiple (OR)
        
        // Returns: WHERE (category_path LIKE ?) OR ...
    }
    
    fn completions(&self, value: &str, cursor: usize) -> Option<(Vec<String>, usize)> {
        // 1. Split value by |, find segment containing cursor
        // 2. Determine anchor_offset (start of that segment)
        // 3. Use segment text as typed prefix for fuzzy matching
        // 4. Exclude other segments from results
        // 5. Return Some((suggestions, anchor_offset)) or None if empty
    }
}
```

### Transfer Filters (Future)

```rust
pub struct FromFilter { pub options: Vec<String> }
pub struct ToFilter { pub options: Vec<String> }

// Same as AccountFilter but with different ids
// Used in transfers_config() instead of AccountFilter
```

## Database View

Create a view with all necessary joins:

```sql
CREATE VIEW transactions_search AS
SELECT 
    t.id,
    t.date,
    t.description,
    t.amount_cents,
    t.balance_cents,
    t.hash,
    t.metadata,
    t.source_file,
    t.import_batch_id,
    t.account_id,
    a.bank_id,
    b.name AS bank_name,
    a.name AS account_name,
    COALESCE(c.path, '') AS category_path,
    te.category_source,
    te.category_confirmed,
    te.ai_confidence
FROM transactions t
JOIN accounts a ON t.account_id = a.id
JOIN banks b ON a.bank_id = b.id
LEFT JOIN transaction_enrichments te ON t.id = te.transaction_id
LEFT JOIN categories c ON te.category_id = c.id;
```

For FTS, we query with a join:

```sql
SELECT ts.* FROM transactions_search ts
JOIN transactions_fts fts ON fts.rowid = ts.id
WHERE fts.description MATCH ?
  AND <other filter clauses>
```

## Search Configuration

```rust
pub struct SearchConfig {
    pub filters: Vec<Box<dyn Filter>>,
    pub allow_regex: bool,
    pub allow_fts: bool,
}

impl SearchConfig {
    pub fn transactions(banks: &[Bank], accounts: &[Account], categories: &[Category]) -> Self {
        Self {
            filters: vec![
                Box::new(DateFilter),
                Box::new(AmountFilter),
                Box::new(AccountFilter::new(banks, accounts)),
                Box::new(CategoryFilter::new(categories)),
            ],
            allow_regex: true,
            allow_fts: true,
        }
    }
    
    pub fn uncategorised(banks: &[Bank], accounts: &[Account]) -> Self {
        Self {
            filters: vec![
                Box::new(DateFilter),
                Box::new(AmountFilter),
                Box::new(AccountFilter::new(banks, accounts)),
                // No CategoryFilter
            ],
            allow_regex: true,
            allow_fts: true,
        }
    }
    
    pub fn transfers(banks: &[Bank], accounts: &[Account]) -> Self {
        Self {
            filters: vec![
                Box::new(DateFilter),
                Box::new(AmountFilter),
                Box::new(FromFilter::new(banks, accounts)),
                Box::new(ToFilter::new(banks, accounts)),
            ],
            allow_regex: true,
            allow_fts: true,
        }
    }
}
```

## Validity Display

Visual feedback for filter validity:

| State | Appearance |
|-------|------------|
| Valid, not selected | Dimmed |
| Invalid, not selected | Red |
| Valid, selected/editing | Full highlighted color, cursor visible |
| Invalid, selected/editing | Dimmed but highlighted color |
| Empty/incomplete | Dimmed |

## Completion Popup Placement

When cursor is in a filter with completions:

1. Check `CursorContext::Filter { name, offset }`
2. Get filter by name, call `completions(value, cursor_offset_in_value)`
3. If `Some((suggestions, anchor_offset))`, show popup
4. Compute screen position: filter token start + `:`.len() + anchor_offset
5. Popup anchors horizontally at that position

The filter returns both suggestions and the anchor offset, so the search bar doesn't need to re-parse the value.

## Parsed Query

```rust
pub struct ParsedQuery {
    pub parts: Vec<QueryPart>,
    pub transition_to_fuzzy: bool,
}

pub enum QueryPart {
    Filter {
        name: &'static str,    // Canonical name (always "date", not "d")
        value: String,
        result: FilterResult,
        span: Span,
        value_span: Span,
    },
    Regex {
        original: String,      // "/pattern/i"
        pattern: String,       // "(?i)pattern"
        valid: bool,
        span: Span,
    },
    Fts {
        original: String,
        query: String,         // With prefix * added
        span: Span,
    },
    Whitespace {
        span: Span,
    },
}

pub struct Span {
    pub start: usize,
    pub end: usize,
}
```

## SQL Query Building

The search system combines all valid filter results:

```rust
impl ParsedQuery {
    pub fn to_sql(&self) -> (String, Vec<rusqlite::types::Value>) {
        let mut clauses = Vec::new();
        let mut params = Vec::new();
        
        for part in &self.parts {
            match part {
                QueryPart::Filter { result: FilterResult::Valid { sql, params: p }, .. } => {
                    clauses.push(sql.clone());
                    params.extend(p.clone());
                }
                QueryPart::Regex { pattern, valid: true, .. } => {
                    clauses.push("description REGEXP ?".to_string());
                    params.push(pattern.clone().into());
                }
                QueryPart::Fts { query, .. } if !query.is_empty() => {
                    // FTS handled separately via JOIN
                }
                _ => {}
            }
        }
        
        let where_clause = if clauses.is_empty() {
            "1=1".to_string()
        } else {
            clauses.join(" AND ")
        };
        
        (where_clause, params)
    }
    
    pub fn fts_query(&self) -> Option<&str> {
        self.parts.iter().find_map(|p| match p {
            QueryPart::Fts { query, .. } if !query.is_empty() => Some(query.as_str()),
            _ => None,
        })
    }
}
```

## File Structure

```
src/search/
├── mod.rs           # Public exports
├── tokenize.rs      # Tokenizer: input → Vec<RawToken>
├── filter.rs        # Filter trait, FilterResult
├── filters/
│   ├── mod.rs
│   ├── date.rs      # DateFilter
│   ├── amount.rs    # AmountFilter
│   ├── account.rs   # AccountFilter
│   └── category.rs  # CategoryFilter
├── query.rs         # ParsedQuery, QueryToken, Span
├── parse.rs         # parse(config, input, cursor) → ParsedQuery
├── config.rs        # SearchConfig
├── context.rs       # CursorContext
└── fts.rs           # FTS query processing (prefix *, paren balancing)

src/tui/
├── search_bar.rs    # SearchBar component
├── app.rs           # Uses SearchBar per tab
└── ui.rs            # Renders SearchBar, completion popup
```

## Migration Steps

### Phase 1: Database view
- [x] Create `transactions_search` view in schema
- [x] Create `transactions_uncategorised` view for todo list
- [x] Create `transfers_search` view with from/to account info
- [ ] Update store queries to use view (optional, can coexist)

### Phase 2: Core types
- [x] Create `src/search/` module structure
- [x] Add `Filter` trait in `search/filter.rs`
- [x] Add `ParsedQuery`, `QueryPart`, `Span` in `search/query.rs`
- [x] Add `CursorContext` in `search/context.rs`
- [x] Move legacy search code to `search/legacy.rs` with re-exports

### Phase 3: Tokenizer
- [x] Create new tokenizer in `search/tokenize.rs`
- [x] Simple rules: filters (name:value), regex (/pattern/flags), FTS (rest)
- [x] Track spans for each token
- [x] RawToken enum: Filter, Regex, Fts, Whitespace with span tracking

### Phase 4: Filter implementations
- [x] Implement `DateFilter` with `..` range syntax
- [x] Implement `AmountFilter` with `..` range syntax
- [x] Implement `AccountFilter` with `|` OR support and completions
- [x] Implement `CategoryFilter` with `|` OR support and completions

### Phase 5: Parser
- [x] Create `parse(config, input, cursor)` in `search/parse.rs`
- [x] Dispatch filter tokens to matching Filter trait impl
- [x] Compute `CursorContext` from cursor position
- [x] SearchConfig holds available filters per context

### Phase 6: SQL building
- [x] Implement `ParsedQuery::to_sql()` returning (where_clause, params)
- [x] Handle FTS via `fts_query()` method for separate JOIN
- [x] Regex patterns use REGEXP function
- [x] Test against existing queries

### Phase 7: SearchBar component
- [ ] Create `SearchBar` in `src/tui/search_bar.rs`
- [ ] Context-aware key handling (`:`, `|`, `/`)
- [ ] Completion popup with anchor positioning
- [ ] Validity-based styling

### Phase 8: Integration
- [ ] Replace `TabSearchState` with `SearchBar`
- [ ] Configure each tab/subtab with appropriate `SearchConfig`
- [ ] Update UI rendering

### Phase 9: Cleanup
- [ ] Remove old `DbSearchQuery` and related code
- [ ] Remove old tokenization code from app.rs
- [ ] Update tests

## Future Enhancements

### Named Filters
```rust
pub struct NamedFilter {
    pub saved_filters: HashMap<String, String>,  // name → query string
}

// filter:tax-deductible&last-fy
// Expands saved filters and combines with &
```

### Transfer Filters
```rust
pub struct FromFilter { /* like AccountFilter */ }
pub struct ToFilter { /* like AccountFilter */ }

// Used in transfers view to filter by source/destination account
```

## Notes

- Span indices are character-based (matching tui_input cursor)
- Filters are stateful (hold completion options) but `parse()` is pure
- FTS uses existing FTS5 infrastructure, just cleaner integration
- Regex uses SQLite REGEXP function (already registered)
