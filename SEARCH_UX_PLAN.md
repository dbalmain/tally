# Search UX Improvements Plan

## Overview

Improve search UX with per-tab state, FTS5 passthrough, smart filter handling, and visual polish.

## Priority Order

### Step 1: Per-tab search state
- Each tab (and Todo subtab) maintains its own DB search and fuzzy search state
- Switching tabs preserves search state for that tab
- Only query/filter the currently visible tab's data
- Reduces unnecessary DB queries

### Step 2: FTS5 passthrough
- Pass FTS query directly to SQLite FTS5 (users learn FTS5 syntax)
- Only modification: add `*` at cursor position for live prefix matching
- Remove custom `|` to `OR` translation - users use native FTS5 `OR`
- Simplifies code, gives power users full FTS5 capabilities

### Step 3: Shortcut expansion
- Expand shortcuts immediately in input text (visible to user):
  - `d:` → `date:`
  - `a:` → `account:`
  - `am:` → `amount:`
  - `c:` → `category:`
- Cursor moves to end of expanded text

### Step 4: Filter deduplication/jump
- When typing a filter that already exists (e.g., second `date:`):
  - Delete the newly typed filter text
  - Move cursor to end of existing filter
  - Append any characters typed after the duplicate filter keyword
- Example: existing `date:2024`, user types `date:2025` → becomes `date:20242025` with cursor at end

### Step 5: Visual dimming
- Dim non-active query sections based on cursor position
- Active section (where cursor is) shown solid
- Filters vs FTS text visually distinguished
- Helps users understand query structure

### Step 6: Auto-reordering filters
- When a filter is typed after FTS terms, move it before FTS portion
- Move cursor along with the filter
- Ensures FTS query is always together at end
- Visual matches semantic parsing

### Step 7: Fuzzy popup for account/category
- Auto-trigger popup when typing `account:` or `category:` value
- Fuzzy-match against available accounts/categories
- On selection, insert value with backslash-escaped spaces (unless user started with `"`)
- For `|` OR syntax: select once, manually add `|` for additional values

## Files to Modify

- `src/tui/app.rs` - per-tab state, filter handling
- `src/tui/mod.rs` - key handling, popup integration
- `src/tui/ui.rs` - rendering, dimming, popups
- `src/search.rs` - FTS5 passthrough, shortcut expansion, filter parsing

## Notes

- Review after each step before proceeding
- Handoff between steps to minimize context
