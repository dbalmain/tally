//! Search system with pluggable filters and context-aware parsing.
//!
//! This module provides a clean, extensible search system with:
//! - Simple tokenization (filters, regex, FTS)
//! - Trait-based pluggable filters that return SQL directly
//! - Context-aware key handling based on cursor position
//!
//! This doc comment is the canonical reference for the query syntax —
//! CLAUDE.md carries only a summary.
//!
//! # Query Syntax
//!
//! **Filters** — `name:value` where the value has no unquoted whitespace:
//!
//! - `date:2024-01-15` — exact date; `date:2024-01` — entire month;
//!   `date:2024` — entire year
//! - `date:2024-01..2024-06` — inclusive range; `date:..2024-06` — up to
//!   the end of June 2024; `date:2024-01..` — from the start of January 2024
//! - Date presets work anywhere a date spec works, including range endpoints:
//!   `date:yesterday`, `date:last-month`, `date:this-quarter`,
//!   `date:last-financial-year`, `date:last-quarter..yesterday`
//! - Relative date presets are the complete periods immediately before the
//!   current one, excluding the in-progress period:
//!   `date:last-7-days`, `date:last-3-months`,
//!   `date:last-2-financial-years`. Supported periods are `days`, `weeks`,
//!   `months`, `quarters`, `years`, and `financial-years`
//! - `amount:100` — precision-aware: any $100-something ($100.00–$100.99);
//!   `amount:7.5` — any $7.5x; `amount:7.50` — exactly $7.50 (two decimals =
//!   exact cents). Matches either sign.
//! - `amount:>100` / `amount:<100` — cent-exact comparison;
//!   `amount:50..200` — range with cent-exact endpoints
//! - Signed matching: an explicit `+`/`-` on any value matches
//!   `amount_cents` directly instead of its absolute value —
//!   `amount:-100..-50` (debits only), `amount:>-5`, `amount:-7` (any
//!   $7-something debit), `amount:-7.50` (exactly -$7.50). A zero endpoint in
//!   a range or comparison is also signed (it is degenerate under ABS):
//!   `amount:0..` / `amount:>0` — credits; `amount:..0` / `amount:<0` —
//!   debits. A bare exact `amount:0` keeps its ABS bucket (under $1, either
//!   sign).
//! - `account:St` — bank prefix; `account:ING/` — all accounts in a bank;
//!   `account:ING/Orange` — bank + account prefix; `account:/Savings` — any
//!   bank, account prefix; `account:"ING/Orange"|"St George/Sav"` — OR
//! - `category:Food` — path starts with "Food"; `category:/Groceries` — a
//!   "Groceries…" segment under any parent (matches after any `/`);
//!   `category:Food|Transport` — OR
//! - `sort:category,amount` — transaction DB searches only: order by the listed
//!   columns in order. Columns are `date`, `description`, `amount`, `balance`,
//!   `account`, `bank`, and `category`. Prefix a column with `-` for descending,
//!   e.g. `sort:-amount`. Ascending is the default for every column. Category
//!   sorting keeps uncategorised rows last. Multiple `sort:` terms are allowed;
//!   the last one wins.
//! - Quoting for values with spaces: `account:"ING/Orange Everyday"` or
//!   `account:ING/Orange\ Everyday`
//!
//! **Regex** — `/pattern/flags` (e.g., `/coffee.*/i` for case-insensitive)
//!
//! **FTS** — everything else is FTS5 full-text search:
//! - `groceries` — simple term (matches word stems)
//! - `coffee shop` — implicit AND
//! - `coffee OR tea` — native FTS5 OR; `(coffee OR tea) breakfast` — grouping
//! - `"exact phrase"` — phrase match; `coff*` — explicit prefix
//! - Live typing adds an implicit `*` at the cursor for prefix matching
//!
//! **Negation** — a leading `-` at a word boundary negates the token that
//! follows it, excluding matching rows:
//!
//! - `-coffee` / `-"asdf"` — exclude rows whose description matches that FTS
//!   term/phrase
//! - `-category:Food`, `-account:ING`, `-amount:>100`, `-date:2024` — exclude
//!   rows matching that filter (`-category:Food` still keeps uncategorised rows,
//!   whose NULL path counts as "did not match")
//! - `-/regex/i` — exclude rows whose description matches the regex
//! - A lone `-` (followed by whitespace or end-of-input) is literal FTS text,
//!   NOT negation. A `-` *inside* a filter value is untouched, so
//!   `amount:-50` stays a signed amount — only a `-` before the whole
//!   `name:value` token negates it.
//! - `-sort:...` is invalid; sorting is not a row match and cannot be negated.
//! - Negation is ignored on transfer searches (a `NOT` is ill-defined across
//!   the "either side matches" OR).
//!
//! **Transition** — end with ` ~` at a word boundary to switch to fuzzy mode
//! while keeping the DB filters.
//!
//! Combined example: `date:2024-01 amount:>100 account:Chase/ sort:-amount groceries`

mod context;
mod filter;
pub mod filters;
mod fuzzy;
mod parse;
pub(crate) mod placeholders;
mod query;
mod render;
mod tokenize;

pub use context::CursorContext;
pub use filter::{Filter, FilterResult};
pub use filters::{AccountFilter, AmountFilter, CategoryFilter, DateFilter, SortFilter};
pub use parse::{SearchConfig, SearchOptions, parse};
pub use query::{ParsedQuery, QueryPart, SortColumn, SortKey, Span};
pub use render::SqlContext;
pub use tokenize::{RawToken, tokenize};

pub use fuzzy::FuzzyMatcher;
