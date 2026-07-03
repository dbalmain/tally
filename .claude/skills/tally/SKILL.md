---
name: tally
description:
  Manage a Tally finance vault from the CLI — list/rename/merge/delete
  categories and (re)assign categories to transactions via `tally` subcommands
  with --json/--csv output. Use when reorganising categories or fixing
  transaction categorisation in a Tally vault.
---

# Tally category & transaction CLI

Tally keeps bank transactions and their categories in a local SQLite database
(`tally.db`) in the vault root. Use these subcommands to inspect and edit
categories and transaction categorisation **without touching SQLite directly**.

## Running

Run from the vault root. Either build once and call the binary:

```
cargo build --release          # one-time, and after any code change
./target/release/tally <args>
```

…or invoke through cargo (its build progress goes to stderr, so stdout stays
clean for `--json`/`--csv`):

```
cargo run -q -- <args>
```

Examples below show `<args>`. Add `--json` for machine-readable output
(preferred for scripting) or `--csv` (list commands only) to export. A global
`--vault PATH` selects a different vault root. On error a command prints a
message to **stderr** and exits non-zero. Any command accepts `--help`/`-h`/`-?`
(printed to stdout, exit 0; `tally help <command>` works too).

## Reading

**`categories list [--json|--csv]`** — every category with its id, path, and
transaction count.

```
cargo run -q -- categories list --json
# [{"id":3,"path":"Food/Groceries","transaction_count":42}, ...]
```

**`transactions list [QUERY…] [--limit N] [--json|--csv]`** — transactions
matching a search QUERY (same syntax as the TUI search bar), newest first;
default limit 100.

```
cargo run -q -- transactions list category:Food amount:>50 --limit 20 --json
# [{"id":42,"date":"2025-01-15","account":"ING/Orange","description":"…",
#   "amount_cents":-1500,"balance_cents":12345,"category":"Food/Groceries"}, ...]
```

QUERY syntax (subset): `date:2024-01..2024-06`, `amount:>100`,
`account:ING/Orange`, `category:Food|Transport`, bare words = full-text search,
`/regex/i` = regex. Narrow with QUERY rather than dumping everything.

## Editing categories

Categories are hierarchical **paths** (`Food/Groceries`); the path is the unique
key. There is no manual ordering — "rearranging" means renaming/moving paths and
merging.

- **`categories rename <path> <new-path>`** — rename one category. Does **not**
  cascade to children: renaming `Food` leaves `Food/Groceries` untouched (rename
  those separately to move a subtree). Errors if `<new-path>` already exists —
  use `merge` for that.
- **`categories merge <source-path> <target-path>`** — move every transaction
  from source to target, then delete source.
- **`categories delete <path> [--force]`** — delete the category; its
  transactions become uncategorised.

**Saved filters stay consistent.** Rename and merge keep any filter that
auto-applies the category pointing at the right one (rename preserves it; merge
repoints it to the target). Delete is the exception: if one or more filters use
the category, `delete` **refuses** and names them — re-run with `--force` to
delete it anyway, which leaves those filters with no category (never a dangling
reference).

```
cargo run -q -- categories rename "Food/Groceries" "Food/Supermarket" --json
cargo run -q -- categories merge "Misc" "Everyday" --json
cargo run -q -- categories delete "Obsolete" --json          # blocked if a filter uses it
cargo run -q -- categories delete "Obsolete" --force --json  # delete + clear those filters
```

## (Re)assigning a transaction

- **`categorise <tx-id> <category-path>`** — set a transaction's category.
  Creates the category if the path is new; replaces any existing category. (A
  transaction is either categorised or part of a transfer, never both.)
- **`categorise <tx-id> --clear`** — remove its category.

Get `<tx-id>` from `transactions list`.

```
cargo run -q -- categorise 42 "Food/Groceries" --json
cargo run -q -- categorise 42 --clear
```

## Typical workflow

1. `categories list --json` — see the current taxonomy and counts.
2. `transactions list <query> --json` — find transactions and their ids.
3. Apply `categorise` and/or `categories rename|merge|delete`.

Each command commits immediately. You can batch several in one shell invocation
(`a && b && c`) for efficiency.
