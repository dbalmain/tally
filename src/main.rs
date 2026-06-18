use std::path::{Path, PathBuf};
use tally::TransactionStore;

struct CliArgs {
    collection: Option<PathBuf>,
    command: Option<String>,
}

fn main() {
    // Initialize file logging: TALLY_LOG=debug cargo run -- tui
    // Logs to ~/.local/share/tally/tally.<date>.log
    match tally::logging::init() {
        Ok(log_dir) => log::info!("Logging to {:?}", log_dir),
        Err(e) => eprintln!("Warning: failed to initialize logging: {}", e),
    }

    let args = parse_cli_args(std::env::args().skip(1));
    let collection_root = args
        .collection
        .or_else(|| std::env::var_os("FM_COLLECTION").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));

    let exports_dir = collection_root.join("exports");
    let db_path = collection_root.join("tally.db");

    // A vault has an `exports/` directory. Bail before opening the store so we
    // don't create a stray tally.db in a directory that isn't a vault.
    if !exports_dir.is_dir() {
        eprintln!("This doesn't appear to be a tally vault");
        std::process::exit(1);
    }

    match args.command.as_deref() {
        Some("tui") | Some("--tui") => run_tui(&db_path, &exports_dir),
        Some("classify") => run_classify(&collection_root, &db_path, &exports_dir),
        _ => run_refresh(&db_path, &exports_dir),
    }
}

fn parse_cli_args(args: impl IntoIterator<Item = String>) -> CliArgs {
    let mut args = args.into_iter();
    let mut collection = None;
    let mut command = None;

    while let Some(arg) = args.next() {
        if arg == "--collection" {
            collection = Some(PathBuf::from(
                args.next().expect("--collection requires a path"),
            ));
        } else if let Some(path) = arg.strip_prefix("--collection=") {
            collection = Some(PathBuf::from(path));
        } else if command.is_none() {
            command = Some(arg);
        }
    }

    CliArgs {
        collection,
        command,
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

fn run_classify(collection_root: &Path, db_path: &Path, exports_dir: &Path) {
    let mut store =
        TransactionStore::open(db_path, exports_dir).expect("Failed to open transaction store");
    let report =
        tally::classify::classify(&mut store, collection_root).expect("Failed to classify");

    println!("Classification complete:");
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
    fn parses_collection_before_command() {
        let args = parse_cli_args(["--collection", "/tmp/finances", "tui"].map(String::from));

        assert_eq!(args.collection, Some(PathBuf::from("/tmp/finances")));
        assert_eq!(args.command.as_deref(), Some("tui"));
    }

    #[test]
    fn parses_collection_after_command() {
        let args = parse_cli_args(["--tui", "--collection=finances"].map(String::from));

        assert_eq!(args.collection, Some(PathBuf::from("finances")));
        assert_eq!(args.command.as_deref(), Some("--tui"));
    }

    #[test]
    fn collection_flag_does_not_change_unknown_command_behavior() {
        let args = parse_cli_args(["refresh", "--collection", "finances"].map(String::from));

        assert_eq!(args.collection, Some(PathBuf::from("finances")));
        assert_eq!(args.command.as_deref(), Some("refresh"));
    }
}
