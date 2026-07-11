mod cli;

use std::path::{Path, PathBuf};
use tally::search::SearchOptions;
use tally::{TransactionStore, config::Config};

struct CliArgs {
    vault: Option<PathBuf>,
    /// All non-`--vault` tokens, in order. The first is the command name.
    rest: Vec<String>,
}

/// Every way the binary can be invoked, classified once from the command name.
///
/// Adding a command family means one new variant here, one arm in
/// [`invocation_for_command`], and one arm in `main`'s dispatch.
///
/// `Help` carries a [`cli::HelpTopic`] (the most-specific command the user asked
/// about). `main` resolves help — printing it and exiting — *before* the vault
/// check, so `--help` on any command works outside a vault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Invocation {
    Tui,
    Refresh,
    Classify,
    Help {
        topic: cli::HelpTopic,
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

    let invocation = invocation_for_command(&args.rest);

    // Help (asked-for or unknown-command) needs no vault at all: resolve it
    // before the vault check so `--help`/`-h`/`-?`/`tally help …` work anywhere.
    if let Invocation::Help { topic } = invocation {
        let to_stderr = topic == cli::HelpTopic::Unknown;
        cli::print_help(topic, to_stderr);
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

    let config = match Config::load(&vault_root) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let search_options = config.search_options();

    match invocation {
        Invocation::Tui => run_tui(&db_path, &exports_dir, search_options),
        Invocation::Refresh => run_refresh(&db_path, &exports_dir, search_options),
        Invocation::Classify => run_classify(&db_path, &exports_dir, search_options),
        Invocation::Cli => run_cli(&args.rest, &db_path, &exports_dir, search_options),
        Invocation::Ai => {
            if let Err(message) = cli::run_ai(&args.rest, &vault_root) {
                eprintln!("{message}");
                std::process::exit(1);
            }
        }
        Invocation::Help { .. } => unreachable!("help returned before vault validation"),
    }
}

fn run_cli(rest: &[String], db_path: &Path, exports_dir: &Path, search_options: SearchOptions) {
    let command = match cli::parse_command(rest) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };

    let mut store =
        TransactionStore::open(db_path, exports_dir).expect("Failed to open transaction store");
    store.set_search_options(search_options);

    if let Err(message) = cli::run_with_search_options(command, &mut store, search_options) {
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
///
/// Help is detected *first* — anywhere among the tokens — so it wins over a
/// would-be parse error (e.g. `tally categories rename --help` with missing
/// args) and over the unknown-command arm.
fn invocation_for_command(rest: &[String]) -> Invocation {
    if let Some(topic) = cli::detect_help(rest) {
        return Invocation::Help { topic };
    }
    match rest.first().map(String::as_str) {
        None | Some("tui") | Some("--tui") => Invocation::Tui,
        Some("pull") => Invocation::Refresh,
        Some("classify") => Invocation::Classify,
        Some(first) if cli::is_cli_command(first) => Invocation::Cli,
        Some(first) if cli::is_ai_command(first) => Invocation::Ai,
        Some(_) => Invocation::Help {
            topic: cli::HelpTopic::Unknown,
        },
    }
}

fn run_refresh(db_path: &Path, exports_dir: &Path, search_options: SearchOptions) {
    let mut store =
        TransactionStore::open(db_path, exports_dir).expect("Failed to open transaction store");
    store.set_search_options(search_options);

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

fn run_tui(db_path: &Path, exports_dir: &Path, search_options: SearchOptions) {
    let mut store =
        TransactionStore::open(db_path, exports_dir).expect("Failed to open transaction store");
    store.set_search_options(search_options);

    if let Err(e) = tally::tui::run(store, search_options) {
        eprintln!("TUI error: {}", e);
        std::process::exit(1);
    }
}

fn run_classify(db_path: &Path, exports_dir: &Path, search_options: SearchOptions) {
    let mut store =
        TransactionStore::open(db_path, exports_dir).expect("Failed to open transaction store");
    store.set_search_options(search_options);
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
        assert_eq!(invocation_for_command(&[]), Invocation::Tui);
        assert_eq!(invocation_for_command(&s(&["tui"])), Invocation::Tui);
        assert_eq!(invocation_for_command(&s(&["--tui"])), Invocation::Tui);
        assert_eq!(invocation_for_command(&s(&["pull"])), Invocation::Refresh);
        assert_eq!(
            invocation_for_command(&s(&["classify"])),
            Invocation::Classify
        );
        assert_eq!(
            invocation_for_command(&s(&["--help"])),
            Invocation::Help {
                topic: cli::HelpTopic::Global
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["-h"])),
            Invocation::Help {
                topic: cli::HelpTopic::Global
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["-?"])),
            Invocation::Help {
                topic: cli::HelpTopic::Global
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["help"])),
            Invocation::Help {
                topic: cli::HelpTopic::Global
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["refresh"])),
            Invocation::Help {
                topic: cli::HelpTopic::Unknown
            }
        );
    }

    #[test]
    fn classifies_cli_and_ai_families() {
        assert_eq!(invocation_for_command(&s(&["categories"])), Invocation::Cli);
        assert_eq!(
            invocation_for_command(&s(&["transactions"])),
            Invocation::Cli
        );
        assert_eq!(invocation_for_command(&s(&["categorise"])), Invocation::Cli);
        assert_eq!(invocation_for_command(&s(&["ai"])), Invocation::Ai);
    }

    #[test]
    fn help_on_any_command_resolves_to_topic() {
        // Family-level help.
        assert_eq!(
            invocation_for_command(&s(&["categories", "--help"])),
            Invocation::Help {
                topic: cli::HelpTopic::Categories
            }
        );
        // Subcommand-level help, mixed with other args.
        assert_eq!(
            invocation_for_command(&s(&["categories", "rename", "A", "B", "--help"])),
            Invocation::Help {
                topic: cli::HelpTopic::CategoriesRename
            }
        );
        // Help wins over a would-be parse error (missing args).
        assert_eq!(
            invocation_for_command(&s(&["categories", "rename", "--help"])),
            Invocation::Help {
                topic: cli::HelpTopic::CategoriesRename
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["categories", "delete", "-?"])),
            Invocation::Help {
                topic: cli::HelpTopic::CategoriesDelete
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["transactions", "list", "--help"])),
            Invocation::Help {
                topic: cli::HelpTopic::TransactionsList
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["categorise", "-h"])),
            Invocation::Help {
                topic: cli::HelpTopic::Categorise
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["ai", "--help"])),
            Invocation::Help {
                topic: cli::HelpTopic::Ai
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["ai", "install-claude-skill", "--help"])),
            Invocation::Help {
                topic: cli::HelpTopic::AiInstallClaudeSkill
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["tui", "--help"])),
            Invocation::Help {
                topic: cli::HelpTopic::Tui
            }
        );
    }

    #[test]
    fn leading_help_form_resolves_topic() {
        assert_eq!(
            invocation_for_command(&s(&["help"])),
            Invocation::Help {
                topic: cli::HelpTopic::Global
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["help", "categories"])),
            Invocation::Help {
                topic: cli::HelpTopic::Categories
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["help", "categories", "rename"])),
            Invocation::Help {
                topic: cli::HelpTopic::CategoriesRename
            }
        );
        assert_eq!(
            invocation_for_command(&s(&["help", "transactions"])),
            Invocation::Help {
                topic: cli::HelpTopic::Transactions
            }
        );
    }

    #[test]
    fn leading_help_unknown_is_unknown_topic() {
        assert_eq!(
            invocation_for_command(&s(&["help", "frobnicate"])),
            Invocation::Help {
                topic: cli::HelpTopic::Unknown
            }
        );
        // A help flag on an unknown command falls back to global help.
        assert_eq!(
            invocation_for_command(&s(&["frobnicate", "--help"])),
            Invocation::Help {
                topic: cli::HelpTopic::Global
            }
        );
    }

    #[test]
    fn help_word_after_command_is_not_help() {
        // `help` must be the FIRST token to trigger help; later it's a query.
        assert_eq!(
            invocation_for_command(&s(&["transactions", "list", "help"])),
            Invocation::Cli
        );
    }

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }
}
