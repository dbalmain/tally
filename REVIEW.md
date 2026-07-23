# REVIEW.md — tally review notes

Repo-specific review guidance, accumulated by the `review-craft` skill. The
skill reads **Standing checks** before every review and appends to the
**Findings log** when a review uncovers a durable lesson. Keep entries terse.

## Standing checks

Mandatory extra criteria every review applies here (promoted from recurring
findings). Each should name the guard that will eventually retire it.

- **TUI dependency bumps (ratatui / crossterm / tui-input) need a render-level
  check.** The suite renders only modal chrome (`modal.rs`), so a changed
  layout/padding/border default passes all gates silently. Until a main-view
  `TestBackend` snapshot exists, treat any such bump as "gates-green ≠ verified"
  and require a manual `cargo run -- tui` pass. _Retire when:_ a Transactions-
  table + tab-bar + detail-panel render test exists.
- **Any change to import or the FTS index must preserve the invariant that
  `transactions_fts` rowid N holds exactly `build_searchable_text` of
  transaction N.** The index is contentless (SQLite never cross-checks postings)
  and drift is silent and additive. _Retire when:_ the invariant test
  (`fts_invariant_holds_after_insert`) is kept green — done as of the 2026-07-24
  entry, so this is now a regression guard rather than an open gap.

## Findings log

### 2026-07-24 — FTS index silently desyncs from transactions (contentless, append-only)

- **What:** `/aami` matched a "Google YouTubePremium" transaction (id 6040) in a
  live vault. Root cause: `transactions_fts` is a contentless FTS5 table
  (`content=''`, `src/db.rs`), and `insert_transaction`
  (`src/store/import.rs:454`) only ever _appended_ a posting at
  `last_insert_rowid()`. Nothing deleted from the index (no rebuild path, no
  sync-on-edit, no trigger), and contentless FTS5 permits multiple postings per
  rowid, so once the id↔row mapping drifts (bulk rebuild / re-import that
  recreates rows / rowid reuse) the index accumulates phantom tokens. 54 rows in
  one vault had false-positive `aami` postings.
- **Why missed:** no earlier review treated the FTS index as state that can
  diverge from its source table. Reviews checked query correctness, never the
  index↔table consistency invariant. The "No Migrations" doc section explicitly
  exempts _views_ ("recreated on every open") but never states that the FTS
  table is NOT similarly rebuilt — the doc's silence hid the gap.
- **Guard:**
  1. _Structural:_ `DELETE FROM transactions_fts WHERE rowid = ?` immediately
     before the insert in `insert_transaction`, making every write idempotent
     and self-healing.
  2. _Structural:_ a `store.rebuild_fts()` (drop+recreate the vtable, repopulate
     via `build_searchable_text`) for one-shot repair; expose via a keybind /
     maintenance command.
  3. _Test (the real guard):_ property/invariant test — import a randomized set
     of transactions, then assert for every row that
     `transactions_fts MATCH <each token of build_searchable_text(row)>` returns
     that rowid AND that the rowid matches _no_ token absent from its own
     searchable text. Re-run after a simulated re-import to catch drift.
  - _Status:_ **applied** on `fix/fts-desync` (commit ed57747, 2026-07-24). All
    three guards landed: idempotent `write_transaction_fts`
    (DELETE-then-INSERT), `store.rebuild_fts()` + TUI `Ctrl-G`, and the
    invariant/drift/rebuild tests (`fts_invariant_holds_after_insert`,
    `fts_drift_heals_on_idempotent_rewrite`,
    `rebuild_fts_repairs_corrupted_index`). Live vaults still need a one-time
    reindex (Ctrl-G or `rebuild_fts`) to clear pre-existing phantom postings.

### 2026-07-24 — dep-upgrade branch has no safety net for the class of change

- **What:** `deps/upgrade-rusqlite-ratatui` bumps rusqlite 0.38→0.40, ratatui
  0.29→0.30, tui-input 0.11→0.15 with **zero source changes** and green gates.
  Correct and minimal, but two guard-gaps make "green" weaker than it looks for
  this class: (a) no render-level test covers the ratatui surface that changed
  (see Standing check 1); (b) clippy strictness (`-D warnings`) lives only in
  the ad-hoc gate command, not in a repo-declared `[lints]` table, so a plain
  `cargo clippy` doesn't enforce it and nothing audits deps for advisories.
- **Guard:**
  1. Add a `[lints.clippy]` table to `Cargo.toml` so strictness is declared
     in-repo and every invocation enforces it (removes reliance on the blessed
     command line).
  2. Add a dependency audit (`cargo-deny` or `cargo-audit`) to a pipeline —
     directly relevant to a dependency-management workflow; nothing currently
     catches a yanked/vulnerable crate.
  3. Render test per Standing check 1.
  - _Status:_ proposed; not applied.
