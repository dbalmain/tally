use std::path::{Path, PathBuf};
use tally::TransactionStore;

struct CliArgs {
    vault: Option<PathBuf>,
    command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Tui,
    Refresh,
    Classify,
    Help { to_stderr: bool },
}

fn main() {
    // Initialize file logging: TALLY_LOG=debug cargo run -- tui
    // Logs to ~/.local/share/tally/tally.<date>.log
    match tally::logging::init() {
        Ok(log_dir) => log::info!("Logging to {:?}", log_dir),
        Err(e) => eprintln!("Warning: failed to initialize logging: {}", e),
    }

    let args = parse_cli_args(std::env::args().skip(1));
    let vault_root = args
        .vault
        .or_else(|| std::env::var_os("FM_VAULT").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));

    let action = action_for_command(args.command.as_deref());

    if let Action::Help { to_stderr } = action {
        print_help(to_stderr);
        if to_stderr {
            std::process::exit(1);
        }
        return;
    }

    let exports_dir = vault_root.join("exports");
    let db_path = vault_root.join("tally.db");

    // A vault has an `exports/` directory. Bail before opening the store so we
    // don't create a stray tally.db in a directory that isn't a vault.
    if !exports_dir.is_dir() {
        eprintln!("This doesn't appear to be a tally vault");
        std::process::exit(1);
    }

    match action {
        Action::Tui => run_tui(&db_path, &exports_dir),
        Action::Refresh => run_refresh(&db_path, &exports_dir),
        Action::Classify => run_classify(&db_path, &exports_dir),
        Action::Help { .. } => unreachable!("help action returned before vault validation"),
    }
}

fn parse_cli_args(args: impl IntoIterator<Item = String>) -> CliArgs {
    let mut args = args.into_iter();
    let mut vault = None;
    let mut command = None;

    while let Some(arg) = args.next() {
        if arg == "--vault" {
            let Some(path) = args.next() else {
                eprintln!("--vault requires a path");
                std::process::exit(1);
            };
            vault = Some(PathBuf::from(path));
        } else if let Some(path) = arg.strip_prefix("--vault=") {
            vault = Some(PathBuf::from(path));
        } else if command.is_none() {
            command = Some(arg);
        }
    }

    CliArgs { vault, command }
}

fn action_for_command(command: Option<&str>) -> Action {
    match command {
        None | Some("tui") | Some("--tui") => Action::Tui,
        Some("pull") => Action::Refresh,
        Some("classify") => Action::Classify,
        Some("--help") | Some("-h") => Action::Help { to_stderr: false },
        Some(_) => Action::Help { to_stderr: true },
    }
}

fn print_help(to_stderr: bool) {
    let help = "\
Usage: tally [--vault PATH] [COMMAND]

Commands:
  (none)     Launch the terminal UI (default)
  pull       Refresh transactions from exports/ (run import/pull scripts)
  classify   Suggest categories and detect transfers locally
  tui        Launch the terminal UI

Global flags:
  --vault PATH   Use PATH as the vault root (or set FM_VAULT)
";

    if to_stderr {
        eprint!("{help}");
    } else {
        print!("{help}");
    }
}

fn run_refresh(db_path: &Path, exports_dir: &Path) {
    let mut store =
        TransactionStore::open(db_path, exports_dir).expect("Failed to open transaction store");

    println!("Refreshing transactions...");
    let report = store.refresh().expect("Failed to refresh");

    println!("Refresh complete:");
    let mut shown = false;
    for (label, count) in [
        ("Banks added", report.banks_added),
        ("Banks deleted", report.banks_deleted),
        ("Accounts added", report.accounts_added),
        ("Accounts deleted", report.accounts_deleted),
        ("Transactions added", report.transactions_added),
    ] {
        if count > 0 {
            println!("  {label}: {count}");
            shown = true;
        }
    }
    if !shown {
        println!("  Nothing new.");
    }

    println!("\nBanks:");
    for bank in store.list_banks().unwrap() {
        println!("  - {} (id: {})", bank.name, bank.id);
        for account in store.list_accounts(bank.id).unwrap() {
            println!("      - {} (id: {})", account.name, account.id);
        }
    }
}

fn run_tui(db_path: &Path, exports_dir: &Path) {
    let store =
        TransactionStore::open(db_path, exports_dir).expect("Failed to open transaction store");

    let (refresh_tx, refresh_rx) = std::sync::mpsc::channel();
    let refresh_db_path = db_path.to_path_buf();
    let refresh_exports_dir = exports_dir.to_path_buf();
    std::thread::spawn(move || {
        let result = TransactionStore::open(&refresh_db_path, &refresh_exports_dir)
            .and_then(|mut store| store.refresh());
        let _ = refresh_tx.send(result);
    });

    if let Err(e) = tally::tui::run(store, refresh_rx) {
        eprintln!("TUI error: {}", e);
        std::process::exit(1);
    }
}

fn run_classify(db_path: &Path, exports_dir: &Path) {
    let mut store =
        TransactionStore::open(db_path, exports_dir).expect("Failed to open transaction store");
    let report = tally::classify::classify(&mut store).expect("Failed to classify");

    println!("Classification complete:");
    println!("  Filter auto-categorised: {}", report.filtered);
    println!("  Transfers detected: {}", report.transfers);
    println!("  Exact-amount suggestions: {}", report.exact);
    println!("  Recurring-biller suggestions: {}", report.recurring);
    println!("  Model suggestions: {}", report.model);
    println!("  Unclassified: {}", report.unclassified);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vault_before_command() {
        let args = parse_cli_args(["--vault", "/tmp/finances", "tui"].map(String::from));

        assert_eq!(args.vault, Some(PathBuf::from("/tmp/finances")));
        assert_eq!(args.command.as_deref(), Some("tui"));
    }

    #[test]
    fn parses_vault_after_command() {
        let args = parse_cli_args(["--tui", "--vault=finances"].map(String::from));

        assert_eq!(args.vault, Some(PathBuf::from("finances")));
        assert_eq!(args.command.as_deref(), Some("--tui"));
    }

    #[test]
    fn vault_flag_does_not_change_unknown_command_behavior() {
        let args = parse_cli_args(["refresh", "--vault", "finances"].map(String::from));

        assert_eq!(args.vault, Some(PathBuf::from("finances")));
        assert_eq!(args.command.as_deref(), Some("refresh"));
    }

    #[test]
    fn resolves_command_actions() {
        assert_eq!(action_for_command(None), Action::Tui);
        assert_eq!(action_for_command(Some("pull")), Action::Refresh);
        assert_eq!(
            action_for_command(Some("refresh")),
            Action::Help { to_stderr: true }
        );
    }
}
