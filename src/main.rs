mod cli;

use std::path::{Path, PathBuf};
use tally::TransactionStore;

struct CliArgs {
    vault: Option<PathBuf>,
    /// All non-`--vault` tokens, in order. The first is the command name.
    rest: Vec<String>,
}

/// Every way the binary can be invoked, classified once from the command name.
///
/// Adding a command family means one new variant here, one arm in
/// [`invocation_for_command`], and one arm in `main`'s dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Invocation {
    Tui,
    Refresh,
    Classify,
    Help {
        to_stderr: bool,
    },
    /// Headless store commands (`categories`/`transactions`/`categorise`).
    Cli,
    /// `ai …` setup commands: act on the vault directory, no store.
    Ai,
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

    let invocation = invocation_for_command(args.rest.first().map(String::as_str));

    // Help (asked-for or unknown-command) needs no vault at all.
    if let Invocation::Help { to_stderr } = invocation {
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

    match invocation {
        Invocation::Tui => run_tui(&db_path, &exports_dir),
        Invocation::Refresh => run_refresh(&db_path, &exports_dir),
        Invocation::Classify => run_classify(&db_path, &exports_dir),
        Invocation::Cli => run_cli(&args.rest, &db_path, &exports_dir),
        Invocation::Ai => {
            if let Err(message) = cli::run_ai(&args.rest, &vault_root) {
                eprintln!("{message}");
                std::process::exit(1);
            }
        }
        Invocation::Help { .. } => unreachable!("help returned before vault validation"),
    }
}

fn run_cli(rest: &[String], db_path: &Path, exports_dir: &Path) {
    let command = match cli::parse_command(rest) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };

    let mut store =
        TransactionStore::open(db_path, exports_dir).expect("Failed to open transaction store");

    if let Err(message) = cli::run(command, &mut store) {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

fn parse_cli_args(args: impl IntoIterator<Item = String>) -> CliArgs {
    let mut args = args.into_iter();
    let mut vault = None;
    let mut rest = Vec::new();

    while let Some(arg) = args.next() {
        if arg == "--vault" {
            let Some(path) = args.next() else {
                eprintln!("--vault requires a path");
                std::process::exit(1);
            };
            vault = Some(PathBuf::from(path));
        } else if let Some(path) = arg.strip_prefix("--vault=") {
            vault = Some(PathBuf::from(path));
        } else {
            rest.push(arg);
        }
    }

    CliArgs { vault, rest }
}

/// The single command-name → family mapping. The `cli` module owns which
/// names belong to its two families, so those arms delegate to its predicates.
fn invocation_for_command(command: Option<&str>) -> Invocation {
    match command {
        None | Some("tui") | Some("--tui") => Invocation::Tui,
        Some("pull") => Invocation::Refresh,
        Some("classify") => Invocation::Classify,
        Some("--help") | Some("-h") => Invocation::Help { to_stderr: false },
        Some(first) if cli::is_cli_command(first) => Invocation::Cli,
        Some(first) if cli::is_ai_command(first) => Invocation::Ai,
        Some(_) => Invocation::Help { to_stderr: true },
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

  categories list [--json|--csv]
             List categories with transaction counts
  categories rename <path> <new-path> [--json]
             Rename a single category (does not cascade to children)
  categories merge <source-path> <target-path> [--json]
             Move source's transactions to target, then delete source
  categories delete <path> [--force] [--json]
             Delete a category (blocked if a filter uses it; --force clears them)

  transactions list [QUERY...] [--limit N] [--json|--csv]
             List transactions matching a search query (default limit 100)

  categorise <tx-id> <category-path> [--json]
             Assign a category to a transaction (created if new)
  categorise <tx-id> --clear [--json]
             Remove a transaction's category

  ai install-claude-skill
             Install the Claude Code skill into this vault's .claude/skills/

Output flags:
  --json         Emit JSON instead of human-readable text
  --csv          Emit CSV (only valid on the `list` commands)
  --limit N      Cap `transactions list` results (default 100)

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
        assert_eq!(args.rest, vec!["tui".to_string()]);
    }

    #[test]
    fn parses_vault_after_command() {
        let args = parse_cli_args(["--tui", "--vault=finances"].map(String::from));

        assert_eq!(args.vault, Some(PathBuf::from("finances")));
        assert_eq!(args.rest, vec!["--tui".to_string()]);
    }

    #[test]
    fn vault_flag_does_not_change_unknown_command_behavior() {
        let args = parse_cli_args(["refresh", "--vault", "finances"].map(String::from));

        assert_eq!(args.vault, Some(PathBuf::from("finances")));
        assert_eq!(args.rest, vec!["refresh".to_string()]);
    }

    #[test]
    fn keeps_all_non_vault_tokens_in_order() {
        let args = parse_cli_args(
            ["categories", "rename", "--vault", "v", "A", "B", "--json"].map(String::from),
        );

        assert_eq!(args.vault, Some(PathBuf::from("v")));
        assert_eq!(
            args.rest,
            vec![
                "categories".to_string(),
                "rename".to_string(),
                "A".to_string(),
                "B".to_string(),
                "--json".to_string(),
            ]
        );
    }

    #[test]
    fn classifies_invocations() {
        assert_eq!(invocation_for_command(None), Invocation::Tui);
        assert_eq!(invocation_for_command(Some("tui")), Invocation::Tui);
        assert_eq!(invocation_for_command(Some("--tui")), Invocation::Tui);
        assert_eq!(invocation_for_command(Some("pull")), Invocation::Refresh);
        assert_eq!(
            invocation_for_command(Some("classify")),
            Invocation::Classify
        );
        assert_eq!(
            invocation_for_command(Some("--help")),
            Invocation::Help { to_stderr: false }
        );
        assert_eq!(
            invocation_for_command(Some("-h")),
            Invocation::Help { to_stderr: false }
        );
        assert_eq!(
            invocation_for_command(Some("refresh")),
            Invocation::Help { to_stderr: true }
        );
    }

    #[test]
    fn classifies_cli_and_ai_families() {
        assert_eq!(invocation_for_command(Some("categories")), Invocation::Cli);
        assert_eq!(
            invocation_for_command(Some("transactions")),
            Invocation::Cli
        );
        assert_eq!(invocation_for_command(Some("categorise")), Invocation::Cli);
        assert_eq!(invocation_for_command(Some("ai")), Invocation::Ai);
    }
}
