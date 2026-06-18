//! Key bindings and key-hint text for the TUI.
//!
//! Normal mode is table-driven here: dispatch, the footer, and the keybind
//! popover all derive from `normal_binds`. Modal modes keep their handlers in
//! `mod.rs`; update their curated hint arms here whenever those handlers change.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::{App, InputMode, Tab, TodoSubTab};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Char(char),
    Code(KeyCode),
}

impl Trigger {
    pub fn matches(self, key: &KeyEvent) -> bool {
        match self {
            Trigger::Char(c) => {
                key.code == KeyCode::Char(c)
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            }
            Trigger::Code(code) => key.code == code,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Quit,
    NextItem,
    PrevItem,
    NextTab,
    PrevTab,
    NextSubtab,
    PrevSubtab,
    DbSearch,
    FuzzySearch,
    Categorise,
    RenameCategory,
    MarkTransfer,
    DeleteTransfer,
    Confirm,
    ClearSearch,
}

#[derive(Debug)]
pub struct Bind {
    pub triggers: &'static [Trigger],
    pub label: &'static str,
    pub desc: &'static str,
    pub footer: bool,
    pub help: bool,
    pub act: Act,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpLine {
    Group(&'static str),
    Bind(&'static str, &'static str),
    Blank,
}

const fn b(
    triggers: &'static [Trigger],
    label: &'static str,
    desc: &'static str,
    footer: bool,
    help: bool,
    act: Act,
) -> Bind {
    Bind {
        triggers,
        label,
        desc,
        footer,
        help,
        act,
    }
}

pub fn normal_binds(app: &App) -> Vec<Bind> {
    use Act::*;
    use Trigger::*;

    let mut out = Vec::new();

    if app.db_search_active() || app.fuzzy_search_active() {
        out.push(b(
            &[Code(KeyCode::Esc)],
            "Esc",
            "clear search",
            true,
            true,
            ClearSearch,
        ));
    }

    if app.selected_transaction().is_some() {
        out.push(b(&[Char('c')], "c", "categorise", true, true, Categorise));
        out.push(b(
            &[Char('t')],
            "t",
            "mark transfer",
            true,
            true,
            MarkTransfer,
        ));
    }

    if app.selected_category().is_some() {
        out.push(b(&[Char('e')], "e", "rename", true, true, RenameCategory));
    }

    if app.current_tab == Tab::Transfers
        && app.lists.linked_transfers.get(app.selected_index).is_some()
    {
        out.push(b(
            &[Char('d')],
            "d",
            "delete transfer",
            true,
            true,
            DeleteTransfer,
        ));
    }

    if let Some(desc) = confirm_desc(app) {
        out.push(b(
            &[Code(KeyCode::Enter)],
            "Enter",
            desc,
            true,
            true,
            Confirm,
        ));
    }

    if app.current_tab == Tab::Todo {
        out.push(b(&[Char(']')], "[ ]", "subtab", true, true, NextSubtab));
        out.push(b(
            &[Char('[')],
            "[",
            "prev subtab",
            false,
            false,
            PrevSubtab,
        ));
    }

    out.push(b(&[Char('/')], "/", "search", false, true, DbSearch));
    out.push(b(
        &[Char('~')],
        "~",
        "fuzzy search",
        false,
        true,
        FuzzySearch,
    ));
    out.push(b(
        &[Code(KeyCode::Tab)],
        "Tab",
        "next tab",
        false,
        true,
        NextTab,
    ));
    out.push(b(
        &[Code(KeyCode::BackTab)],
        "S-Tab",
        "prev tab",
        false,
        true,
        PrevTab,
    ));
    out.push(b(
        &[Char('j'), Code(KeyCode::Down)],
        "j/↓",
        "down",
        false,
        true,
        NextItem,
    ));
    out.push(b(
        &[Char('k'), Code(KeyCode::Up)],
        "k/↑",
        "up",
        false,
        true,
        PrevItem,
    ));
    out.push(b(&[Char('q')], "q", "quit", false, true, Quit));

    out
}

fn confirm_desc(app: &App) -> Option<&'static str> {
    if app.current_tab != Tab::Todo {
        return None;
    }
    match app.todo_subtab {
        TodoSubTab::AiReview if app.lists.ai_reviews.get(app.selected_index).is_some() => {
            Some("confirm category")
        }
        TodoSubTab::TransferReview
            if app.lists.transfer_reviews.get(app.selected_index).is_some() =>
        {
            Some("confirm transfer")
        }
        _ => None,
    }
}

pub fn dispatch_normal(app: &mut App, key: KeyEvent) {
    if key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT) {
        run_normal(app, Act::PrevTab);
        return;
    }

    let act = normal_binds(app)
        .iter()
        .find(|bind| bind.triggers.iter().any(|trigger| trigger.matches(&key)))
        .map(|bind| bind.act);
    if let Some(act) = act {
        run_normal(app, act);
    }
}

fn run_normal(app: &mut App, act: Act) {
    match act {
        Act::Quit => app.should_quit = true,
        Act::NextItem => app.next(),
        Act::PrevItem => app.previous(),
        Act::NextTab => app.next_tab(),
        Act::PrevTab => app.previous_tab(),
        Act::NextSubtab => app.next_subtab(),
        Act::PrevSubtab => app.previous_subtab(),
        Act::DbSearch => app.start_db_search(),
        Act::FuzzySearch => app.start_fuzzy_search(),
        Act::Categorise => app.start_category_edit(),
        Act::RenameCategory => app.start_category_rename(),
        Act::MarkTransfer => app.start_transfer_mark(),
        Act::DeleteTransfer => app.delete_transfer(),
        Act::Confirm => {
            app.confirm_ai_category();
            app.confirm_transfer_review();
        }
        Act::ClearSearch => app.clear_search(),
    }
}

pub fn footer_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    match app.input_mode {
        InputMode::Normal => {
            let mut out: Vec<_> = normal_binds(app)
                .iter()
                .filter(|bind| bind.footer)
                .map(|bind| (bind.label, bind.desc))
                .collect();
            out.push(("?", "keys"));
            out
        }
        InputMode::DbSearch if app.filter_autocomplete_active() => {
            vec![("↑/↓", "select"), ("Tab/Enter", "accept"), ("Esc", "close")]
        }
        InputMode::DbSearch => vec![("Enter", "apply"), ("Esc", "clear & exit")],
        InputMode::FuzzySearch => vec![("Enter", "keep filter"), ("Esc", "clear & exit")],
        InputMode::Category => vec![("↑/↓", "select"), ("Enter", "assign"), ("Esc", "cancel")],
        InputMode::CategoryEdit => vec![("Enter", "save"), ("Esc", "cancel")],
        InputMode::ConfirmMerge => with_keys(vec![("y", "merge"), ("n", "cancel")]),
        InputMode::TransferPending => with_keys(vec![
            ("↑/↓", "select"),
            ("T/Enter", "link"),
            ("t", "re-search"),
            ("Esc", "cancel"),
        ]),
        InputMode::TransferNoMatch => with_keys(vec![("Esc", "dismiss")]),
    }
}

fn with_keys(mut hints: Vec<(&'static str, &'static str)>) -> Vec<(&'static str, &'static str)> {
    hints.push(("?", "keys"));
    hints
}

pub fn help_lines(app: &App) -> Vec<HelpLine> {
    let mut lines = Vec::new();
    match app.input_mode {
        InputMode::Normal => normal_lines(app, &mut lines),
        InputMode::DbSearch => db_search_lines(app, &mut lines),
        InputMode::FuzzySearch => fuzzy_search_lines(&mut lines),
        InputMode::Category => category_lines(&mut lines),
        InputMode::CategoryEdit => category_edit_lines(&mut lines),
        InputMode::ConfirmMerge => confirm_merge_lines(&mut lines),
        InputMode::TransferPending => transfer_pending_lines(&mut lines),
        InputMode::TransferNoMatch => transfer_no_match_lines(&mut lines),
    }
    lines
}

fn normal_lines(app: &App, lines: &mut Vec<HelpLine>) {
    group(lines, "Keys");
    for bind in normal_binds(app) {
        if bind.help {
            bind_line(lines, bind.label, bind.desc);
        }
    }
}

fn db_search_lines(app: &App, lines: &mut Vec<HelpLine>) {
    if app.filter_autocomplete_active() {
        group(lines, "Autocomplete");
        bind_line(lines, "↑/↓", "select suggestion");
        bind_line(lines, "Tab/Enter", "accept suggestion");
        bind_line(lines, "Esc", "close suggestions");
    }
    group(lines, "Search");
    bind_line(lines, "Type", "edit query");
    bind_line(lines, "Backspace/Delete", "delete text");
    bind_line(lines, "Arrows/Home/End", "move cursor");
    bind_line(lines, "Enter", "apply");
    bind_line(lines, "Esc", "clear & exit");
    bind_line(lines, "Tab/S-Tab", "switch tab");
}

fn fuzzy_search_lines(lines: &mut Vec<HelpLine>) {
    group(lines, "Fuzzy Search");
    bind_line(lines, "Type", "edit filter");
    bind_line(lines, "Backspace/Delete", "delete text");
    bind_line(lines, "Arrows/Home/End", "move cursor");
    bind_line(lines, "↑/↓", "move selection");
    bind_line(lines, "Enter", "keep filter");
    bind_line(lines, "Esc", "clear & exit");
    bind_line(lines, "Tab/S-Tab", "switch tab");
}

fn category_lines(lines: &mut Vec<HelpLine>) {
    group(lines, "Category");
    bind_line(lines, "Type", "filter or create category");
    bind_line(lines, "Backspace", "delete text");
    bind_line(lines, "↑/↓", "select suggestion");
    bind_line(lines, "Enter", "assign");
    bind_line(lines, "Esc", "cancel");
}

fn category_edit_lines(lines: &mut Vec<HelpLine>) {
    group(lines, "Rename Category");
    bind_line(lines, "Type", "edit path");
    bind_line(lines, "Arrows/Home/End", "move cursor");
    bind_line(lines, "Backspace/Delete", "delete text");
    bind_line(lines, "Enter", "save");
    bind_line(lines, "Esc", "cancel");
}

fn confirm_merge_lines(lines: &mut Vec<HelpLine>) {
    group(lines, "Merge Category");
    bind_line(lines, "y/Enter", "merge");
    bind_line(lines, "n/Esc", "cancel");
}

fn transfer_pending_lines(lines: &mut Vec<HelpLine>) {
    group(lines, "Transfer");
    bind_line(lines, "↑/↓ or j/k", "select candidate");
    bind_line(lines, "T/Enter", "link");
    bind_line(lines, "t", "re-search");
    bind_line(lines, "Esc", "cancel");
}

fn transfer_no_match_lines(lines: &mut Vec<HelpLine>) {
    group(lines, "Transfer");
    bind_line(lines, "Esc", "dismiss");
}

fn group(lines: &mut Vec<HelpLine>, title: &'static str) {
    if !lines.is_empty() {
        lines.push(HelpLine::Blank);
    }
    lines.push(HelpLine::Group(title));
}

fn bind_line(lines: &mut Vec<HelpLine>, key: &'static str, desc: &'static str) {
    lines.push(HelpLine::Bind(key, desc));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::filtered_list::FilteredList;
    use crate::{
        Category, Transaction, TransactionStore, TransactionWithEnrichment, Transfer,
        TransferSource, TransferWithTransactions,
    };
    use chrono::{NaiveDate, Utc};

    #[test]
    fn normal_binds_have_no_duplicate_triggers_per_context() {
        for (tab, subtab) in contexts() {
            let mut app = app_with_rows();
            app.current_tab = tab;
            app.todo_subtab = subtab;

            let mut seen = Vec::new();
            for bind in normal_binds(&app) {
                for trigger in bind.triggers {
                    assert!(
                        !seen.contains(trigger),
                        "duplicate trigger {:?} in {:?}/{:?}",
                        trigger,
                        tab,
                        subtab
                    );
                    seen.push(*trigger);
                }
            }
        }
    }

    #[test]
    fn normal_binds_are_context_honest() {
        let mut app = app_with_rows();
        app.current_tab = Tab::Categories;
        let binds = normal_binds(&app);
        assert!(has_act_trigger(
            &binds,
            Act::RenameCategory,
            Trigger::Char('e')
        ));
        assert!(!has_act(&binds, Act::DeleteTransfer));

        app.current_tab = Tab::Transfers;
        let binds = normal_binds(&app);
        assert!(has_act_trigger(
            &binds,
            Act::DeleteTransfer,
            Trigger::Char('d')
        ));

        app.current_tab = Tab::Todo;
        app.todo_subtab = TodoSubTab::AiReview;
        let binds = normal_binds(&app);
        let bind = find_act(&binds, Act::Confirm).unwrap();
        assert_eq!(bind.desc, "confirm category");
        assert!(bind.triggers.contains(&Trigger::Code(KeyCode::Enter)));

        app.todo_subtab = TodoSubTab::TransferReview;
        let binds = normal_binds(&app);
        let bind = find_act(&binds, Act::Confirm).unwrap();
        assert_eq!(bind.desc, "confirm transfer");
        assert!(bind.triggers.contains(&Trigger::Code(KeyCode::Enter)));

        app.lists.transfer_reviews = FilteredList::default();
        assert!(!has_act(&normal_binds(&app), Act::Confirm));
    }

    #[test]
    fn normal_footer_hints_map_to_dispatch_binds() {
        for (tab, subtab) in contexts() {
            let mut app = app_with_rows();
            app.current_tab = tab;
            app.todo_subtab = subtab;
            app.input_mode = InputMode::Normal;

            let binds = normal_binds(&app);
            for (label, desc) in footer_hints(&app) {
                if label == "?" {
                    continue;
                }
                assert!(
                    binds
                        .iter()
                        .any(|bind| bind.footer && bind.label == label && bind.desc == desc),
                    "footer hint {label} {desc} has no bind in {:?}/{:?}",
                    tab,
                    subtab
                );
            }
        }
    }

    fn contexts() -> [(Tab, TodoSubTab); 6] {
        [
            (Tab::Transactions, TodoSubTab::Uncategorised),
            (Tab::Transfers, TodoSubTab::Uncategorised),
            (Tab::Categories, TodoSubTab::Uncategorised),
            (Tab::Todo, TodoSubTab::Uncategorised),
            (Tab::Todo, TodoSubTab::AiReview),
            (Tab::Todo, TodoSubTab::TransferReview),
        ]
    }

    fn app_with_rows() -> App {
        let temp = tempfile::tempdir().unwrap();
        let store = TransactionStore::open_in_memory(temp.path()).unwrap();
        let mut app = App::new(store).unwrap();

        let tx1 = tx(1, -1200);
        let tx2 = tx(2, 1200);
        let category = category(1);
        let pending_transfer = transfer(1, tx1.id, tx2.id, false);
        let linked_transfer = transfer(2, tx1.id, tx2.id, true);

        app.lists.transactions = FilteredList::new(vec![tx1.clone()]);
        app.lists.uncategorised = FilteredList::new(vec![tx1.clone()]);
        app.lists.ai_reviews = FilteredList::new(vec![TransactionWithEnrichment {
            transaction: tx1.clone(),
            enrichment: None,
            category: Some(category.clone()),
        }]);
        app.lists.transfer_reviews = FilteredList::new(vec![pending_transfer]);
        app.lists.linked_transfers = FilteredList::new(vec![TransferWithTransactions {
            transfer: linked_transfer,
            from_transaction: tx1,
            to_transaction: tx2,
        }]);
        app.lists.categories = FilteredList::new(vec![category]);
        app
    }

    fn tx(id: i64, amount_cents: i64) -> Transaction {
        Transaction {
            id,
            bank_id: 1,
            account_id: 1,
            date: NaiveDate::from_ymd_opt(2024, 1, id as u32).unwrap(),
            description: format!("Transaction {id}"),
            amount_cents,
            balance_cents: 10_000,
            hash: format!("hash-{id}"),
            metadata: Default::default(),
            source_file: "test.csv".to_string(),
            import_batch_id: 1,
        }
    }

    fn category(id: i64) -> Category {
        Category {
            id,
            path: "Food/Groceries".to_string(),
            created_at: Utc::now(),
        }
    }

    fn transfer(
        id: i64,
        from_transaction_id: i64,
        to_transaction_id: i64,
        confirmed: bool,
    ) -> Transfer {
        Transfer {
            id,
            from_transaction_id,
            to_transaction_id,
            source: TransferSource::Manual,
            confirmed,
            ai_confidence: None,
            created_at: Utc::now(),
        }
    }

    fn find_act(binds: &[Bind], act: Act) -> Option<&Bind> {
        binds.iter().find(|bind| bind.act == act)
    }

    fn has_act(binds: &[Bind], act: Act) -> bool {
        find_act(binds, act).is_some()
    }

    fn has_act_trigger(binds: &[Bind], act: Act, trigger: Trigger) -> bool {
        find_act(binds, act).is_some_and(|bind| bind.triggers.contains(&trigger))
    }
}
