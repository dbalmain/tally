# Contributing to Tally

**Keep documentation up-to-date.** When adding features or changing architecture, update AGENTS.md and README.md as needed.

## Development Setup

```bash
git clone <repo>
cd tally
cargo build
cargo test
```

## Project Structure

```
src/
├── main.rs         # CLI entry, argument parsing
├── lib.rs          # Public API exports
├── types.rs        # Core data structures (Transaction, Bank, Account, etc.)
├── db.rs           # SQLite schema initialization
├── store.rs        # TransactionStore: all database operations
├── import.rs       # Import script execution, file discovery, hashing
├── error.rs        # Error types
└── tui/
    ├── mod.rs      # TUI entry point, event loop, key handling
    ├── app.rs      # Application state and actions
    └── ui.rs       # Rendering functions
```

## Key Conventions

### Money Representation
All monetary values use `i64` cents to avoid floating-point errors:
```rust
amount_cents: i64,  // $123.45 = 12345, -$50.00 = -5000
```

### Error Handling
Use the `Result` type alias from `error.rs`. Propagate errors with `?`.

### Database
- No migrations—schema changes require deleting `tally.db` and re-importing
- Soft deletes for banks/accounts (set `deleted_at` timestamp)
- All queries go through `TransactionStore` in `store.rs`

### TUI Guidelines
- **Whitespace over borders** — Avoid box borders on tables
- **Context over labels** — Use tabs for context, don't repeat headers
- **Color palette:**
  - Red: negative amounts, debits
  - Green: positive amounts, credits
  - Yellow: categories, warnings
  - Cyan: transfers, metadata
  - DarkGray: labels, disabled items

## Testing

```bash
cargo test              # Run all tests
cargo test <test_name>  # Run specific test
```

Tests use in-memory SQLite databases via `TransactionStore::open_in_memory()`.

## Making Changes

1. Check existing patterns in neighboring code before adding new features
2. Keep functions small and focused
3. Avoid adding comments unless the code is genuinely complex
4. Run `cargo build` and `cargo test` before committing

## Adding a New Feature

### New Store Method
1. Add method to `TransactionStore` in `store.rs`
2. Add any new types to `types.rs`
3. Export new types in `lib.rs` if public

### New TUI View
1. Add state to `App` in `app.rs`
2. Add rendering function in `ui.rs`
3. Add key handlers in `mod.rs`
4. Update `draw()` to call your render function

### New Tab or Subtab
1. Add variant to `Tab` or `TodoSubTab` enum in `app.rs`
2. Update `all()` and `title()` methods
3. Add rendering in `ui.rs`
4. Handle selection/navigation in `mod.rs`
