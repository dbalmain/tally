# tools

Generic, checked-in helper tools for Tally. Nothing here contains
account-specific data — all per-account state (folders, scripts, logs) lives
under the gitignored `exports/` directory.

## pocketsmith-pull

Fetches transactions from [PocketSmith](https://www.pocketsmith.com/) into
Tally's import format.

### Setup

1. Create a PocketSmith developer key: Profile → Security & integrations →
   Manage developer keys.
2. Export it as `POCKETSMITH_KEY` (e.g. in `.envrc`):

   ```bash
   export POCKETSMITH_KEY=your_key_here
   ```

### Usage

```bash
# Discover your accounts and scaffold exports/<Bank>/<Account>/ folders,
# each with a generated `pull` shim. Safe to re-run: accounts already wired
# up (under any folder name) are skipped.
tools/pocketsmith-pull sync

# Import everything into tally.db. Tally runs each folder's `pull` shim
# during refresh.
cargo run
```

You can rename the generated folders however you like — the shims key off the
PocketSmith account id, not the folder name, and a re-`sync` won't recreate a
renamed folder.

### How it works

- `sync` lists your accounts via the API and writes a `pull` shim into each
  `exports/<Bank>/<Account>/` folder. Duplicate account names are
  disambiguated with the account id.
- Each `pull` shim calls `pocketsmith-pull account <id>`, which Tally executes
  during refresh. It emits Tally-import JSON on stdout and appends a line to
  `pull.log` in the folder:

  ```
  <timestamp>\t<from>\t<to>\t<count>
  ```

- Pulls are incremental: the next run reads the last `to` date and re-fetches
  from `to − 14 days` (an overlap that catches out-of-order transactions). The
  first run (no log) fetches the account's full history.
- Re-fetched rows are deduped by a stable `hash` of `ps-<pocketsmith_id>`, via
  Tally's `UNIQUE(account_id, hash)` constraint — so overlap never duplicates.
- PocketSmith's category is parked in `metadata.pocketsmith_category` as an
  autocategorisation hint. It is **not** applied to Tally's own category
  system.

### Requirements

Ruby (standard library only). No gems needed.
