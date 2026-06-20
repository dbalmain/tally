# tools

Generic, checked-in helper tools for Tally. Nothing here contains
account-specific data — all per-account state (folders, scripts, logs) lives in
the separate collection repository configured by `FM_ROOT`.

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

3. Set `FM_ROOT` to the root of the private repository containing your finance
   collections:

   ```bash
   export FM_ROOT="$HOME/fm"
   ```

4. Add a `pocketsmith.toml` whitelist to each collection that should receive
   PocketSmith accounts. Collections are immediate subdirectories of
   `FM_ROOT`:

   ```toml
   # $FM_ROOT/personal/pocketsmith.toml
   accounts = [
     123456,
     789012,
   ]
   ```

   Account ids may appear in only one collection. The parser accepts integer
   ids, comments, multi-line arrays, and trailing commas; no TOML gem is
   required.

### Usage

```bash
# Show PocketSmith account ids and their whitelist assignments.
tools/pocketsmith-pull list

# Scaffold a generated pull shim for each whitelisted account at
# $FM_ROOT/<collection>/exports/<Bank>/<Account>/pull. Safe to re-run:
# accounts already wired up anywhere under FM_ROOT are skipped.
tools/pocketsmith-pull sync

# Import one collection. Tally runs each account folder's `pull` shim.
cargo run -- --vault "$FM_ROOT/personal" pull
```

You can rename the generated folders however you like — the shims key off the
PocketSmith account id, not the folder name, and a re-`sync` won't recreate a
renamed folder. Accounts not listed in any `pocketsmith.toml` are reported and
skipped.

### How it works

- `list` lists accounts from the API as
  `<id>  <bank> / <account name>  -> <collection or (unassigned)>`, making it
  easy to find ids for the collection whitelists.
- `sync` lists accounts via the API and writes a `pull` shim for each
  whitelisted id into
  `$FM_ROOT/<collection>/exports/<Bank>/<Account>/`. Duplicate account names
  are disambiguated with the account id. An id assigned to multiple
  collections is an error.
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
