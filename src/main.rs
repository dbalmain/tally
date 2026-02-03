use std::path::{Path, PathBuf};
use tally::TransactionStore;

fn main() {
    // Initialize file logging: TALLY_LOG=debug cargo run -- tui
    // Logs to ~/.local/share/tally/tally.<date>.log
    match tally::logging::init() {
        Ok(log_dir) => log::info!("Logging to {:?}", log_dir),
        Err(e) => eprintln!("Warning: failed to initialize logging: {}", e),
    }

    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).map(|s| s.as_str());

    let exports_dir = PathBuf::from("exports");
    let db_path = PathBuf::from("tally.db");

    match command {
        Some("tui") | Some("--tui") => run_tui(&db_path, &exports_dir),
        _ => run_refresh(&db_path, &exports_dir),
    }
}

fn run_refresh(db_path: &Path, exports_dir: &Path) {
    let mut store = TransactionStore::open(db_path, exports_dir)
        .expect("Failed to open transaction store");

    println!("Refreshing transactions...");
    let report = store.refresh().expect("Failed to refresh");

    println!("Refresh complete:");
    println!("  Banks added: {}", report.banks_added);
    println!("  Banks deleted: {}", report.banks_deleted);
    println!("  Accounts added: {}", report.accounts_added);
    println!("  Accounts deleted: {}", report.accounts_deleted);
    println!("  Files processed: {}", report.files_processed);
    println!("  Transactions added: {}", report.transactions_added);
    println!("  Transactions skipped: {}", report.transactions_skipped);

    println!("\nBanks:");
    for bank in store.list_banks().unwrap() {
        println!("  - {} (id: {})", bank.name, bank.id);
        for account in store.list_accounts(bank.id).unwrap() {
            println!("      - {} (id: {})", account.name, account.id);
        }
    }
}

fn run_tui(db_path: &Path, exports_dir: &Path) {
    let mut store = TransactionStore::open(db_path, exports_dir)
        .expect("Failed to open transaction store");

    // Auto-refresh on TUI startup
    if let Err(e) = store.refresh() {
        eprintln!("Warning: refresh failed: {}", e);
    }

    if let Err(e) = tally::tui::run(store) {
        eprintln!("TUI error: {}", e);
        std::process::exit(1);
    }
}
