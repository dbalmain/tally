# Tally

Personal transaction aggregator that imports bank CSV exports into a SQLite database for querying, categorization, and reporting.

## Features

- Import transactions from any bank via customizable import scripts
- Automatic deduplication of transactions
- Terminal UI for browsing and managing transactions
- Category management with manual and AI-assisted categorization
- Transfer detection and linking between accounts
- SQLite storage for easy querying and portability

## Installation

```bash
git clone <repo>
cd tally
cargo build --release
```

## Quick Start

1. **Create your exports directory structure:**
   ```
   exports/
   └── YourBank/
       └── Checking/
           ├── import          # Script to parse this bank's CSV format
           └── transactions.csv
   ```

2. **Write an import script** for your bank's CSV format (see [Import Scripts](#import-scripts))

3. **Import transactions:**
   ```bash
   cargo run
   ```

4. **Launch the TUI:**
   ```bash
   cargo run -- --tui
   ```

## Import Scripts

Each account needs an `import` script that converts the bank's CSV format to JSON. The script:
- Receives the CSV file path as an argument
- Outputs a JSON array to stdout
- Runs in the CSV file's directory

### Output Format

```json
[
  {
    "date": "2025-01-15",
    "description": "ACME Corp Payment",
    "amount_cents": -15000,
    "balance_cents": 123456
  }
]
```

**Required fields:**
- `date` — ISO format (YYYY-MM-DD)
- `description` — Transaction description
- `amount_cents` — Amount in cents (negative for debits)
- `balance_cents` — Account balance after transaction, in cents

**Optional fields:**
- `hash` — Custom deduplication hash (auto-computed if missing)
- `metadata` — Arbitrary JSON object for extra data

### Example Import Script (Python)

```python
#!/usr/bin/env python3
import csv, json, sys

with open(sys.argv[1]) as f:
    reader = csv.DictReader(f)
    txs = []
    for row in reader:
        txs.append({
            "date": row["Date"],
            "description": row["Description"],
            "amount_cents": int(float(row["Amount"]) * 100),
            "balance_cents": int(float(row["Balance"]) * 100),
        })
    print(json.dumps(txs))
```

## TUI Usage

| Key | Action |
|-----|--------|
| `Tab` / `Shift+Tab` | Switch tabs |
| `[` / `]` | Switch subtabs (in Todo) |
| `j` / `k` or arrows | Navigate list |
| `c` | Set category for transaction |
| `t` | Start transfer linking |
| `T` or `Enter` | Confirm transfer link |
| `d` | Delete transfer (in Transfers tab) |
| `Enter` | Confirm AI category / transfer review |
| `Esc` | Cancel current action |
| `q` | Quit |

### Tabs

- **Transactions** — Browse all transactions
- **Transfers** — View linked transfers between accounts
- **Todo** — Items needing attention:
  - *Uncategorised* — Transactions without categories
  - *AI Review* — AI-suggested categories to confirm
  - *Transfer Review* — Auto-detected transfers to confirm

## License

MIT
