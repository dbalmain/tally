//! Headless CLI subcommands for category management and transaction listing.
//!
//! These let an external agent inspect and restructure categories and reassign
//! transactions without touching SQLite directly. The parser is a pure function
//! (`parse_command`) so it can be unit-tested; the executors take a
//! `&mut TransactionStore`.
//!
//! `main.rs` strips `--vault`/`--vault=` before handing the remaining tokens to
//! `parse_command`, so the slice here never contains a vault flag.

use std::path::Path;

use tally::{CategorySource, Error, TransactionStore};

/// The canonical Claude Code skill, embedded at build time so an installed
/// binary can write it into any vault via `tally ai install-claude-skill`.
const CLAUDE_SKILL_MD: &str = include_str!("../.claude/skills/tally/SKILL.md");

/// Where the skill lives inside a vault, relative to its root.
const CLAUDE_SKILL_REL: &str = ".claude/skills/tally/SKILL.md";

/// How a command's result should be rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
    Csv,
}

/// A parsed CLI subcommand ready to execute against the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    CategoriesList {
        format: Format,
    },
    CategoryRename {
        from: String,
        to: String,
        json: bool,
    },
    CategoryMerge {
        source: String,
        target: String,
        json: bool,
    },
    CategoryDelete {
        path: String,
        json: bool,
        force: bool,
    },
    TransactionsList {
        query: String,
        limit: usize,
        format: Format,
    },
    Categorise {
        tx_id: i64,
        path: String,
        json: bool,
    },
    Uncategorise {
        tx_id: i64,
        json: bool,
    },
}

const DEFAULT_LIMIT: usize = 100;

// ==================== Contextual help ====================
//
// Single source of truth for every command's usage line(s) and one-line
// description. Global help, per-command help, and parse-error messages all
// render from the `SPECS` table below so the three can never drift.

/// A help topic: the most-specific command named by the user's tokens.
///
/// `Global` is the full top-level help; `Unknown` is the same text but printed
/// to stderr with a non-zero exit (used for unknown commands and the
/// `tally help <unknown>` form).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpTopic {
    Global,
    Unknown,
    Tui,
    Pull,
    Classify,
    Categories,
    CategoriesList,
    CategoriesRename,
    CategoriesMerge,
    CategoriesDelete,
    Transactions,
    TransactionsList,
    Categorise,
    Ai,
    AiInstallClaudeSkill,
}

/// One usage form of a command (most commands have exactly one; `categorise`
/// has two — set and clear).
#[derive(Clone, Copy)]
struct Form {
    usage: &'static str,
    desc: &'static str,
}

/// A documented flag for a command.
#[derive(Clone, Copy)]
struct FlagSpec {
    name: &'static str,
    desc: &'static str,
}

/// One row of the command table. Family nodes (paths like `["categories"]`)
/// have empty `forms`/`flags`; their leaves carry the actual usage and flags.
struct Spec {
    topic: HelpTopic,
    /// Full command path, e.g. `["categories", "list"]`. Single element for
    /// top-level/standalone commands.
    path: &'static [&'static str],
    /// Usage forms. Empty for family nodes (their leaves carry the forms).
    forms: &'static [Form],
    /// Documented flags. Empty for family nodes.
    flags: &'static [FlagSpec],
}

const FLAG_JSON: FlagSpec = FlagSpec {
    name: "--json",
    desc: "Emit JSON instead of human-readable text",
};
const FLAG_CSV: FlagSpec = FlagSpec {
    name: "--csv",
    desc: "Emit CSV (only valid on `list` commands)",
};
const FLAG_FORCE: FlagSpec = FlagSpec {
    name: "--force",
    desc: "Delete even if a filter uses the category (clears those filters)",
};
const FLAG_LIMIT: FlagSpec = FlagSpec {
    name: "--limit N",
    desc: "Cap results (default 100)",
};

const SPECS: &[Spec] = &[
    Spec {
        topic: HelpTopic::Tui,
        path: &["tui"],
        forms: &[Form {
            usage: "tui",
            desc: "Launch the terminal UI (also the default command)",
        }],
        flags: &[],
    },
    Spec {
        topic: HelpTopic::Pull,
        path: &["pull"],
        forms: &[Form {
            usage: "pull",
            desc: "Refresh transactions from exports/ (run import/pull scripts)",
        }],
        flags: &[],
    },
    Spec {
        topic: HelpTopic::Classify,
        path: &["classify"],
        forms: &[Form {
            usage: "classify",
            desc: "Suggest categories and detect transfers locally",
        }],
        flags: &[],
    },
    Spec {
        topic: HelpTopic::Categories,
        path: &["categories"],
        forms: &[],
        flags: &[],
    },
    Spec {
        topic: HelpTopic::CategoriesList,
        path: &["categories", "list"],
        forms: &[Form {
            usage: "categories list [--json|--csv]",
            desc: "List categories with transaction counts",
        }],
        flags: &[FLAG_JSON, FLAG_CSV],
    },
    Spec {
        topic: HelpTopic::CategoriesRename,
        path: &["categories", "rename"],
        forms: &[Form {
            usage: "categories rename <path> <new-path> [--json]",
            desc: "Rename a single category (does not cascade to children)",
        }],
        flags: &[FLAG_JSON],
    },
    Spec {
        topic: HelpTopic::CategoriesMerge,
        path: &["categories", "merge"],
        forms: &[Form {
            usage: "categories merge <source-path> <target-path> [--json]",
            desc: "Move source's transactions to target, then delete source",
        }],
        flags: &[FLAG_JSON],
    },
    Spec {
        topic: HelpTopic::CategoriesDelete,
        path: &["categories", "delete"],
        forms: &[Form {
            usage: "categories delete <path> [--force] [--json]",
            desc: "Delete a category (blocked if a filter uses it; --force clears them)",
        }],
        flags: &[FLAG_FORCE, FLAG_JSON],
    },
    Spec {
        topic: HelpTopic::Transactions,
        path: &["transactions"],
        forms: &[],
        flags: &[],
    },
    Spec {
        topic: HelpTopic::TransactionsList,
        path: &["transactions", "list"],
        forms: &[Form {
            usage: "transactions list [QUERY...] [--limit N] [--json|--csv]",
            desc: "List transactions matching a search query (default limit 100)",
        }],
        flags: &[FLAG_JSON, FLAG_CSV, FLAG_LIMIT],
    },
    Spec {
        topic: HelpTopic::Categorise,
        path: &["categorise"],
        forms: &[
            Form {
                usage: "categorise <tx-id> <category-path> [--json]",
                desc: "Assign a category to a transaction (created if new)",
            },
            Form {
                usage: "categorise <tx-id> --clear [--json]",
                desc: "Remove a transaction's category",
            },
        ],
        flags: &[FLAG_JSON],
    },
    Spec {
        topic: HelpTopic::Ai,
        path: &["ai"],
        forms: &[],
        flags: &[],
    },
    Spec {
        topic: HelpTopic::AiInstallClaudeSkill,
        path: &["ai", "install-claude-skill"],
        forms: &[Form {
            usage: "ai install-claude-skill",
            desc: "Install the Claude Code skill into this vault's .claude/skills/",
        }],
        flags: &[],
    },
];

fn find_spec(topic: HelpTopic) -> &'static Spec {
    SPECS
        .iter()
        .find(|s| s.topic == topic)
        .expect("every HelpTopic has a Spec")
}

fn command_path(topic: HelpTopic) -> String {
    find_spec(topic).path.join(" ")
}

fn form_usage(topic: HelpTopic, idx: usize) -> &'static str {
    let spec = find_spec(topic);
    spec.forms.get(idx).map(|f| f.usage).unwrap_or_else(|| {
        spec.forms
            .first()
            .expect("non-family spec has a form")
            .usage
    })
}

/// Resolve `tokens` (the command path, with help tokens already stripped) to the
/// most-specific known [`HelpTopic`]. Empty tokens → `Global`. Non-empty tokens
/// that don't even match a known root → `Unknown`.
fn resolve_topic(tokens: &[&str]) -> HelpTopic {
    if tokens.is_empty() {
        return HelpTopic::Global;
    }
    let mut best: Option<&Spec> = None;
    for spec in SPECS {
        if spec.path.len() <= tokens.len() && spec.path.iter().zip(tokens).all(|(a, b)| a == b) {
            match best {
                None => best = Some(spec),
                Some(cur) if cur.path.len() < spec.path.len() => best = Some(spec),
                _ => {}
            }
        }
    }
    match best {
        Some(spec) => spec.topic,
        None => HelpTopic::Unknown,
    }
}

/// Detect a help request anywhere in the vault-stripped token slice.
///
/// Two forms are recognised:
/// - a leading `help` token (`tally help`, `tally help categories rename`);
/// - any of `-h`, `--help`, `-?` among the tokens (`tally categories --help`).
///
/// A bare `help` appearing later (e.g. `tally transactions list help`) is NOT
/// help — it keeps its current meaning (there, an FTS search for "help").
pub fn detect_help(args: &[String]) -> Option<HelpTopic> {
    if args.first().map(String::as_str) == Some("help") {
        let tokens: Vec<&str> = args[1..].iter().map(String::as_str).collect();
        return Some(resolve_topic(&tokens));
    }
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "-h" | "--help" | "-?"))
    {
        let tokens: Vec<&str> = args
            .iter()
            .filter(|a| !matches!(a.as_str(), "-h" | "--help" | "-?"))
            .map(String::as_str)
            .collect();
        let topic = resolve_topic(&tokens);
        // A help flag is always a request for help, never an unknown-command
        // error, so an unmatched path falls back to the global help (stdout).
        return Some(if topic == HelpTopic::Unknown {
            HelpTopic::Global
        } else {
            topic
        });
    }
    None
}

/// Render the help text for `topic`. `main` owns the destination (stdout/stderr)
/// and the exit code; this is pure data.
pub fn render_help(topic: HelpTopic) -> String {
    match topic {
        HelpTopic::Global | HelpTopic::Unknown => render_global(),
        HelpTopic::Categories | HelpTopic::Transactions | HelpTopic::Ai => render_family(topic),
        other => render_leaf(other),
    }
}

/// Print the rendered help. `to_stderr` selects the stream; `main` decides it
/// and the exit code.
pub fn print_help(topic: HelpTopic, to_stderr: bool) {
    let text = render_help(topic);
    if to_stderr {
        eprint!("{text}");
    } else {
        print!("{text}");
    }
}

/// "Usage: tally <usage>\n\nsee: tally <path> --help" — the single-sourced
/// shape consumed by parse errors.
fn usage_error(topic: HelpTopic, form_idx: usize) -> String {
    format!(
        "Usage: tally {}\n\nsee: tally {} --help",
        form_usage(topic, form_idx),
        command_path(topic),
    )
}

// Global help, composed from the same `SPECS` table so it can never drift.
fn render_global() -> String {
    let mut out = String::new();
    out.push_str("Usage: tally [--vault PATH] [COMMAND]\n\nCommands:\n");

    // Top-level inline group: the default and the three standalone commands.
    out.push_str("  (none)     Launch the terminal UI (default)\n");
    for topic in [HelpTopic::Pull, HelpTopic::Classify, HelpTopic::Tui] {
        let spec = find_spec(topic);
        out.push_str(&format!("  {:<11} {}\n", spec.path[0], spec.forms[0].desc));
    }

    out.push('\n');

    // Multi-word commands: usage on one line, description indented under it.
    for topic in [
        HelpTopic::CategoriesList,
        HelpTopic::CategoriesRename,
        HelpTopic::CategoriesMerge,
        HelpTopic::CategoriesDelete,
    ] {
        let spec = find_spec(topic);
        out.push_str(&format!("  {}\n", spec.forms[0].usage));
        out.push_str(&format!("             {}\n", spec.forms[0].desc));
    }

    out.push('\n');

    let tx = find_spec(HelpTopic::TransactionsList);
    out.push_str(&format!("  {}\n", tx.forms[0].usage));
    out.push_str(&format!("             {}\n\n", tx.forms[0].desc));

    let cat = find_spec(HelpTopic::Categorise);
    for form in cat.forms {
        out.push_str(&format!("  {}\n", form.usage));
        out.push_str(&format!("             {}\n", form.desc));
    }

    out.push('\n');

    let ai = find_spec(HelpTopic::AiInstallClaudeSkill);
    out.push_str(&format!("  {}\n", ai.forms[0].usage));
    out.push_str(&format!(
        "             {}\n\nOutput flags:\n  --json         Emit JSON instead of human-readable text\n  --csv          Emit CSV (only valid on the `list` commands)\n  --limit N      Cap `transactions list` results (default 100)\n\nGlobal flags:\n  --vault PATH   Use PATH as the vault root (or set FM_VAULT)\n",
        ai.forms[0].desc
    ));

    out
}

fn render_leaf(topic: HelpTopic) -> String {
    let spec = find_spec(topic);
    let mut out = String::new();
    if spec.forms.len() == 1 {
        out.push_str(&format!(
            "Usage: tally {}\n\n{}\n",
            spec.forms[0].usage, spec.forms[0].desc
        ));
    } else {
        out.push_str("Usage:\n");
        for form in spec.forms {
            out.push_str(&format!("  tally {}\n    {}\n", form.usage, form.desc));
        }
        out.push('\n');
    }
    if !spec.flags.is_empty() {
        out.push_str("Flags:\n");
        let width = spec.flags.iter().map(|f| f.name.len()).max().unwrap_or(0);
        for flag in spec.flags {
            out.push_str(&format!(
                "  {:<width$}  {}\n",
                flag.name,
                flag.desc,
                width = width
            ));
        }
    }
    out
}

fn render_family(topic: HelpTopic) -> String {
    let family = find_spec(topic);
    let leaves: Vec<&Spec> = SPECS
        .iter()
        .filter(|s| s.path.len() > family.path.len() && s.path.starts_with(family.path))
        .collect();

    let mut out = String::new();
    out.push_str(&format!(
        "Usage: tally {} <subcommand> [...]\n\nSubcommands:\n",
        family.path[0]
    ));
    for leaf in &leaves {
        for form in leaf.forms {
            out.push_str(&format!(
                "  tally {}\n              {}\n",
                form.usage, form.desc
            ));
        }
    }

    // Union of flag descriptions across the family, deduped by name.
    let mut seen: Vec<&str> = Vec::new();
    let mut flags: Vec<FlagSpec> = Vec::new();
    for leaf in &leaves {
        for flag in leaf.flags {
            if !seen.contains(&flag.name) {
                seen.push(flag.name);
                flags.push(*flag);
            }
        }
    }
    out.push('\n');
    if !flags.is_empty() {
        out.push_str("Flags:\n");
        let width = flags.iter().map(|f| f.name.len()).max().unwrap_or(0);
        for flag in flags {
            out.push_str(&format!(
                "  {:<width$}  {}\n",
                flag.name,
                flag.desc,
                width = width
            ));
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "Run `tally {} <subcommand> --help` for a subcommand's flag details.\n",
        family.path[0]
    ));
    out
}

/// True if `first` names an `ai …` setup command. These act on the vault
/// directory rather than the store, so `main` routes them to [`run_ai`].
pub fn is_ai_command(first: &str) -> bool {
    first == "ai"
}

/// Run an `ai …` subcommand against the vault directory (no store needed).
/// `args` is the full token slice, so `args[0]` is `"ai"`.
pub fn run_ai(args: &[String], vault_root: &Path) -> Result<(), String> {
    match args.get(1).map(String::as_str) {
        Some("install-claude-skill") => {
            if let Some(extra) = args.get(2) {
                return Err(format!("Unexpected argument: {extra}"));
            }
            install_claude_skill(vault_root)
        }
        Some(other) => Err(format!(
            "Unknown ai subcommand: {other} (expected install-claude-skill)\n\nsee: tally ai --help"
        )),
        None => Err(
            "ai requires a subcommand (install-claude-skill)\n\nsee: tally ai --help".to_string(),
        ),
    }
}

/// Write the embedded Claude skill into `<vault>/.claude/skills/tally/SKILL.md`,
/// creating parent directories. Overwrites any existing copy so re-running picks
/// up a newer binary's skill text.
fn install_claude_skill(vault_root: &Path) -> Result<(), String> {
    let path = vault_root.join(CLAUDE_SKILL_REL);
    let dir = path
        .parent()
        .expect("CLAUDE_SKILL_REL has a parent directory");
    std::fs::create_dir_all(dir).map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
    let existed = path.exists();
    std::fs::write(&path, CLAUDE_SKILL_MD)
        .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    println!(
        "{} Claude skill at {}",
        if existed { "Updated" } else { "Installed" },
        path.display()
    );
    Ok(())
}

/// True if `first` names one of the CLI subcommands handled here.
pub fn is_cli_command(first: &str) -> bool {
    matches!(first, "categories" | "transactions" | "categorise")
}

/// Output flags shared by every command, collected in one pass.
struct Flags {
    json: bool,
    csv: bool,
    limit: Option<usize>,
    positional: Vec<String>,
}

/// Split `args` into output flags and positional tokens.
///
/// `allow_limit` gates `--limit`; `allow_csv` gates `--csv`. A token of `--`
/// is not special-cased — these commands take no values that look like flags.
fn collect_flags(args: &[String], allow_limit: bool, allow_csv: bool) -> Result<Flags, String> {
    let mut flags = Flags {
        json: false,
        csv: false,
        limit: None,
        positional: Vec::new(),
    };

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => flags.json = true,
            "--csv" => {
                if !allow_csv {
                    return Err("--csv is only valid on `list` commands".to_string());
                }
                flags.csv = true;
            }
            "--limit" if allow_limit => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--limit requires a number".to_string())?;
                flags.limit = Some(parse_limit(value)?);
            }
            other if allow_limit && other.starts_with("--limit=") => {
                let value = other.trim_start_matches("--limit=");
                flags.limit = Some(parse_limit(value)?);
            }
            other if other.starts_with("--") => {
                return Err(format!("Unknown flag: {other}"));
            }
            other => flags.positional.push(other.to_string()),
        }
    }

    if flags.json && flags.csv {
        return Err("--json and --csv are mutually exclusive".to_string());
    }

    Ok(flags)
}

fn parse_limit(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .map_err(|_| format!("Invalid --limit value: {value}"))
}

/// Resolve the output format from collected flags (for `list` commands).
fn format_of(flags: &Flags) -> Format {
    if flags.json {
        Format::Json
    } else if flags.csv {
        Format::Csv
    } else {
        Format::Text
    }
}

/// Parse the already-vault-stripped argument slice into a [`Command`].
///
/// `args[0]` is the top-level command (`categories`/`transactions`/
/// `categorise`); the rest are its arguments and flags.
pub fn parse_command(args: &[String]) -> Result<Command, String> {
    let (head, rest) = args
        .split_first()
        .ok_or_else(|| "No command given".to_string())?;

    match head.as_str() {
        "categories" => parse_categories(rest),
        "transactions" => parse_transactions(rest),
        "categorise" => parse_categorise(rest),
        other => Err(format!("Unknown command: {other}")),
    }
}

fn parse_categories(args: &[String]) -> Result<Command, String> {
    let (sub, rest) = args.split_first().ok_or_else(|| {
        "categories requires a subcommand (list|rename|merge|delete)\n\nsee: tally categories --help"
            .to_string()
    })?;

    match sub.as_str() {
        "list" => {
            let flags = collect_flags(rest, false, true)?;
            expect_positional(
                &flags.positional,
                0,
                &usage_error(HelpTopic::CategoriesList, 0),
            )?;
            Ok(Command::CategoriesList {
                format: format_of(&flags),
            })
        }
        "rename" => {
            let flags = collect_flags(rest, false, false)?;
            let [from, to] = take_two(
                &flags.positional,
                &usage_error(HelpTopic::CategoriesRename, 0),
            )?;
            Ok(Command::CategoryRename {
                from,
                to,
                json: flags.json,
            })
        }
        "merge" => {
            let flags = collect_flags(rest, false, false)?;
            let [source, target] = take_two(
                &flags.positional,
                &usage_error(HelpTopic::CategoriesMerge, 0),
            )?;
            Ok(Command::CategoryMerge {
                source,
                target,
                json: flags.json,
            })
        }
        "delete" => parse_categories_delete(rest),
        other => Err(format!(
            "Unknown categories subcommand: {other} (expected list|rename|merge|delete)\n\nsee: tally categories --help"
        )),
    }
}

fn parse_categories_delete(args: &[String]) -> Result<Command, String> {
    // `delete` carries a `--force` flag alongside its positional, so it can't
    // use the shared flag collector (which rejects unknown `--` flags).
    let mut json = false;
    let mut force = false;
    let mut positional = Vec::new();

    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--force" => force = true,
            "--csv" => return Err("--csv is only valid on `list` commands".to_string()),
            other if other.starts_with("--") => return Err(format!("Unknown flag: {other}")),
            other => positional.push(other.to_string()),
        }
    }

    let [path] = take_one(&positional, &usage_error(HelpTopic::CategoriesDelete, 0))?;
    Ok(Command::CategoryDelete { path, json, force })
}

fn parse_transactions(args: &[String]) -> Result<Command, String> {
    let (sub, rest) = args.split_first().ok_or_else(|| {
        "transactions requires a subcommand (list)\n\nsee: tally transactions --help".to_string()
    })?;

    match sub.as_str() {
        "list" => {
            let flags = collect_flags(rest, true, true)?;
            Ok(Command::TransactionsList {
                query: flags.positional.join(" "),
                limit: flags.limit.unwrap_or(DEFAULT_LIMIT),
                format: format_of(&flags),
            })
        }
        other => Err(format!(
            "Unknown transactions subcommand: {other} (expected list)\n\nsee: tally transactions --help"
        )),
    }
}

fn parse_categorise(args: &[String]) -> Result<Command, String> {
    // `categorise` carries a `--clear` flag alongside its positionals, so it
    // can't use the shared flag collector (which rejects unknown `--` flags).
    let mut clear = false;
    let mut json = false;
    let mut positional = Vec::new();

    for arg in args {
        match arg.as_str() {
            "--clear" => clear = true,
            "--json" => json = true,
            "--csv" => return Err("--csv is only valid on `list` commands".to_string()),
            other if other.starts_with("--") => return Err(format!("Unknown flag: {other}")),
            other => positional.push(other.to_string()),
        }
    }

    if clear {
        let [id] = take_one(&positional, &usage_error(HelpTopic::Categorise, 1))?;
        let tx_id = parse_tx_id(&id)?;
        return Ok(Command::Uncategorise { tx_id, json });
    }

    let [id, path] = take_two(&positional, &usage_error(HelpTopic::Categorise, 0))?;
    let tx_id = parse_tx_id(&id)?;
    Ok(Command::Categorise { tx_id, path, json })
}

fn parse_tx_id(value: &str) -> Result<i64, String> {
    value
        .parse::<i64>()
        .map_err(|_| format!("Invalid transaction id: {value}"))
}

/// `err` is the fully-formed error message (typically from [`usage_error`]).
fn expect_positional(positional: &[String], n: usize, err: &str) -> Result<(), String> {
    if positional.len() == n {
        Ok(())
    } else {
        Err(err.to_string())
    }
}

fn take_one(positional: &[String], err: &str) -> Result<[String; 1], String> {
    match positional {
        [a] => Ok([a.clone()]),
        _ => Err(err.to_string()),
    }
}

fn take_two(positional: &[String], err: &str) -> Result<[String; 2], String> {
    match positional {
        [a, b] => Ok([a.clone(), b.clone()]),
        _ => Err(err.to_string()),
    }
}

// ==================== Execution ====================

/// Run a parsed command against the store, printing its output to stdout.
pub fn run(command: Command, store: &mut TransactionStore) -> Result<(), String> {
    match command {
        Command::CategoriesList { format } => categories_list(store, format),
        Command::CategoryRename { from, to, json } => category_rename(store, &from, &to, json),
        Command::CategoryMerge {
            source,
            target,
            json,
        } => category_merge(store, &source, &target, json),
        Command::CategoryDelete { path, json, force } => category_delete(store, &path, json, force),
        Command::TransactionsList {
            query,
            limit,
            format,
        } => transactions_list(store, &query, limit, format),
        Command::Categorise { tx_id, path, json } => categorise(store, tx_id, &path, json),
        Command::Uncategorise { tx_id, json } => uncategorise(store, tx_id, json),
    }
}

/// Resolve a category path to its id, erroring if it does not exist.
fn resolve_category(store: &TransactionStore, path: &str) -> Result<i64, String> {
    match store.get_category_by_path(path).map_err(stringify)? {
        Some(category) => Ok(category.id),
        None => Err(format!("Category \"{path}\" not found")),
    }
}

fn categories_list(store: &TransactionStore, format: Format) -> Result<(), String> {
    let categories = store.list_categories().map_err(stringify)?;
    let counts = store.get_category_transaction_counts().map_err(stringify)?;

    match format {
        Format::Json => {
            let rows: Vec<_> = categories
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "id": c.id,
                        "path": c.path,
                        "transaction_count": counts.get(&c.id).copied().unwrap_or(0),
                    })
                })
                .collect();
            print_json(&serde_json::Value::Array(rows));
        }
        Format::Csv => {
            print!("{}", csv_row(&["id", "path", "transaction_count"]));
            for c in &categories {
                let count = counts.get(&c.id).copied().unwrap_or(0);
                print!(
                    "{}",
                    csv_row(&[&c.id.to_string(), &c.path, &count.to_string()])
                );
            }
        }
        Format::Text => {
            let count_width = categories
                .iter()
                .map(|c| counts.get(&c.id).copied().unwrap_or(0).to_string().len())
                .max()
                .unwrap_or(1);
            for c in &categories {
                let count = counts.get(&c.id).copied().unwrap_or(0);
                println!("{:>count_width$}  {}  (id {})", count, c.path, c.id);
            }
        }
    }
    Ok(())
}

fn category_rename(
    store: &mut TransactionStore,
    from: &str,
    to: &str,
    json: bool,
) -> Result<(), String> {
    let id = resolve_category(store, from)?;
    match store.rename_category(id, to) {
        Ok(()) => {}
        Err(Error::CategoryExists(_)) => {
            return Err(format!(
                "Category \"{to}\" already exists; use `tally categories merge` to combine them."
            ));
        }
        Err(e) => return Err(stringify(e)),
    }

    if json {
        print_json(&serde_json::json!({ "action": "rename", "from": from, "to": to }));
    } else {
        println!("Renamed \"{from}\" → \"{to}\"");
    }
    Ok(())
}

fn category_merge(
    store: &mut TransactionStore,
    source: &str,
    target: &str,
    json: bool,
) -> Result<(), String> {
    let source_id = resolve_category(store, source)?;
    let target_id = resolve_category(store, target)?;
    if source_id == target_id {
        return Err("source and target are the same category".to_string());
    }
    let moved = store
        .count_transactions_in_category(source_id)
        .map_err(stringify)?;
    let repointed = store
        .filters_using_category(source_id)
        .map_err(stringify)?
        .len();
    store
        .merge_categories(source_id, target_id)
        .map_err(stringify)?;

    if json {
        print_json(&serde_json::json!({
            "action": "merge",
            "source": source,
            "target": target,
            "moved": moved,
            "filters_repointed": repointed,
        }));
    } else {
        let mut message =
            format!("Merged \"{source}\" into \"{target}\" ({moved} transactions moved)");
        if repointed > 0 {
            message.push_str(&format!(", {repointed} filters repointed"));
        }
        println!("{message}");
    }
    Ok(())
}

fn category_delete(
    store: &mut TransactionStore,
    path: &str,
    json: bool,
    force: bool,
) -> Result<(), String> {
    let id = resolve_category(store, path)?;
    let affected = store.filters_using_category(id).map_err(stringify)?;

    if !affected.is_empty() && !force {
        let names = affected
            .iter()
            .map(|f| format!("\"{}\"", f.name))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "Category \"{path}\" is used by {} filter(s): {names}. Re-run with --force to delete it and clear those filters.",
            affected.len()
        ));
    }

    let filters_cleared = affected.len();
    let uncategorised = store.delete_category(id).map_err(stringify)?;

    if json {
        print_json(&serde_json::json!({
            "action": "delete",
            "path": path,
            "uncategorised": uncategorised,
            "filters_cleared": filters_cleared,
        }));
    } else {
        let mut message =
            format!("Deleted \"{path}\" ({uncategorised} transactions uncategorised)");
        if filters_cleared > 0 {
            message.push_str(&format!(", {filters_cleared} filters cleared"));
        }
        println!("{message}");
    }
    Ok(())
}

fn transactions_list(
    store: &TransactionStore,
    query: &str,
    limit: usize,
    format: Format,
) -> Result<(), String> {
    let config = tally::search::SearchConfig::standard(Vec::new(), Some(Vec::new()));
    let parsed = if query.trim().is_empty() {
        tally::search::ParsedQuery::empty()
    } else {
        tally::search::parse(&config, query, 0).0
    };

    let transactions = store
        .query_transactions(&parsed, Some(limit))
        .map_err(stringify)?;

    // Build id->label maps once.
    let mut bank_names = std::collections::HashMap::new();
    let mut account_names = std::collections::HashMap::new();
    for bank in store.list_banks().map_err(stringify)? {
        for account in store.list_accounts(bank.id).map_err(stringify)? {
            account_names.insert(account.id, account.name);
        }
        bank_names.insert(bank.id, bank.name);
    }

    let ids: Vec<i64> = transactions.iter().map(|t| t.id).collect();
    let categories = store
        .get_categories_for_transactions(&ids)
        .map_err(stringify)?;

    let account_label = |tx: &tally::Transaction| {
        let bank = bank_names
            .get(&tx.bank_id)
            .map(String::as_str)
            .unwrap_or("?");
        let account = account_names
            .get(&tx.account_id)
            .map(String::as_str)
            .unwrap_or("?");
        format!("{bank}/{account}")
    };

    match format {
        Format::Json => {
            let rows: Vec<_> = transactions
                .iter()
                .map(|tx| {
                    serde_json::json!({
                        "id": tx.id,
                        "date": tx.date.to_string(),
                        "account": account_label(tx),
                        "description": tx.description,
                        "amount_cents": tx.amount_cents,
                        "balance_cents": tx.balance_cents,
                        "category": categories.get(&tx.id),
                    })
                })
                .collect();
            print_json(&serde_json::Value::Array(rows));
        }
        Format::Csv => {
            print!(
                "{}",
                csv_row(&[
                    "id",
                    "date",
                    "account",
                    "description",
                    "amount_cents",
                    "balance_cents",
                    "category",
                ])
            );
            for tx in &transactions {
                print!(
                    "{}",
                    csv_row(&[
                        &tx.id.to_string(),
                        &tx.date.to_string(),
                        &account_label(tx),
                        &tx.description,
                        &tx.amount_cents.to_string(),
                        &tx.balance_cents.to_string(),
                        categories.get(&tx.id).map(String::as_str).unwrap_or(""),
                    ])
                );
            }
        }
        Format::Text => {
            for tx in &transactions {
                let category = categories.get(&tx.id).map(String::as_str).unwrap_or("");
                println!(
                    "{}  {:>12}  {:<40}  {}",
                    tx.date,
                    format_amount(tx.amount_cents),
                    truncate(&tx.description, 40),
                    category
                );
            }
        }
    }
    Ok(())
}

fn categorise(
    store: &mut TransactionStore,
    tx_id: i64,
    path: &str,
    json: bool,
) -> Result<(), String> {
    require_transaction(store, tx_id)?;
    let cat_id = store.get_or_create_category(path).map_err(stringify)?;
    store
        .set_category(tx_id, cat_id, CategorySource::Manual, true, None)
        .map_err(stringify)?;

    if json {
        print_json(&serde_json::json!({
            "action": "categorise",
            "transaction_id": tx_id,
            "category": path,
        }));
    } else {
        println!("Categorised transaction {tx_id} as \"{path}\"");
    }
    Ok(())
}

fn uncategorise(store: &mut TransactionStore, tx_id: i64, json: bool) -> Result<(), String> {
    require_transaction(store, tx_id)?;
    store.delete_enrichment(tx_id).map_err(stringify)?;

    if json {
        print_json(&serde_json::json!({
            "action": "uncategorise",
            "transaction_id": tx_id,
        }));
    } else {
        println!("Cleared category from transaction {tx_id}");
    }
    Ok(())
}

fn require_transaction(store: &TransactionStore, tx_id: i64) -> Result<(), String> {
    let found = store.get_transactions_by_ids(&[tx_id]).map_err(stringify)?;
    if found.contains_key(&tx_id) {
        Ok(())
    } else {
        Err(format!("Transaction {tx_id} not found"))
    }
}

// ==================== Formatting helpers ====================

fn stringify(error: Error) -> String {
    error.to_string()
}

fn print_json(value: &serde_json::Value) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(e) => eprintln!("Failed to serialise JSON: {e}"),
    }
}

fn format_amount(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Quote a single CSV field per RFC 4180: wrap in double quotes iff it contains
/// a comma, double-quote, CR, or LF; embedded double-quotes are doubled.
fn csv_field(field: &str) -> String {
    if field.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Render one CSV record (trailing `\r\n` per RFC 4180).
fn csv_row(fields: &[&str]) -> String {
    let mut row = fields
        .iter()
        .map(|f| csv_field(f))
        .collect::<Vec<_>>()
        .join(",");
    row.push_str("\r\n");
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tally::Category;
    use tempfile::TempDir;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    // ---------- help detection tests ----------

    #[test]
    fn detects_help_flag_at_each_level() {
        use HelpTopic::*;
        assert_eq!(detect_help(&s(&["--help"])), Some(Global));
        assert_eq!(detect_help(&s(&["-h"])), Some(Global));
        assert_eq!(detect_help(&s(&["-?"])), Some(Global));
        assert_eq!(detect_help(&s(&["tui", "--help"])), Some(Tui));
        assert_eq!(detect_help(&s(&["pull", "-h"])), Some(Pull));
        assert_eq!(detect_help(&s(&["classify", "-?"])), Some(Classify));
        assert_eq!(detect_help(&s(&["categories", "--help"])), Some(Categories));
        assert_eq!(
            detect_help(&s(&["categories", "list", "--help"])),
            Some(CategoriesList)
        );
        assert_eq!(
            detect_help(&s(&["categories", "rename", "--help"])),
            Some(CategoriesRename)
        );
        assert_eq!(
            detect_help(&s(&["categories", "merge", "--help"])),
            Some(CategoriesMerge)
        );
        assert_eq!(
            detect_help(&s(&["categories", "delete", "--help"])),
            Some(CategoriesDelete)
        );
        assert_eq!(
            detect_help(&s(&["transactions", "--help"])),
            Some(Transactions)
        );
        assert_eq!(
            detect_help(&s(&["transactions", "list", "--help"])),
            Some(TransactionsList)
        );
        assert_eq!(detect_help(&s(&["categorise", "--help"])), Some(Categorise));
        assert_eq!(detect_help(&s(&["ai", "--help"])), Some(Ai));
        assert_eq!(
            detect_help(&s(&["ai", "install-claude-skill", "--help"])),
            Some(AiInstallClaudeSkill)
        );
    }

    #[test]
    fn detects_leading_help_form() {
        use HelpTopic::*;
        assert_eq!(detect_help(&s(&["help"])), Some(Global));
        assert_eq!(detect_help(&s(&["help", "categories"])), Some(Categories));
        assert_eq!(
            detect_help(&s(&["help", "categories", "rename"])),
            Some(CategoriesRename)
        );
        assert_eq!(
            detect_help(&s(&["help", "transactions", "list"])),
            Some(TransactionsList)
        );
        assert_eq!(detect_help(&s(&["help", "tui"])), Some(Tui));
        assert_eq!(
            detect_help(&s(&["help", "ai", "install-claude-skill"])),
            Some(AiInstallClaudeSkill)
        );
    }

    #[test]
    fn help_flag_mixed_with_args_still_resolves() {
        assert_eq!(
            detect_help(&s(&["categories", "rename", "A", "B", "--help"])),
            Some(HelpTopic::CategoriesRename)
        );
    }

    #[test]
    fn help_flag_unknown_command_falls_back_to_global() {
        assert_eq!(
            detect_help(&s(&["frobnicate", "--help"])),
            Some(HelpTopic::Global)
        );
    }

    #[test]
    fn leading_help_unknown_command_is_unknown() {
        assert_eq!(
            detect_help(&s(&["help", "frobnicate"])),
            Some(HelpTopic::Unknown)
        );
    }

    #[test]
    fn help_word_after_first_token_is_not_help() {
        // `help` must be the FIRST token to count; later it's a normal token.
        assert_eq!(detect_help(&s(&["transactions", "list", "help"])), None);
        // A categories "help" subcommand is not help either.
        assert_eq!(detect_help(&s(&["categories", "help"])), None);
    }

    #[test]
    fn render_leaf_includes_usage_desc_and_flags() {
        let text = render_help(HelpTopic::CategoriesDelete);
        assert!(
            text.contains("Usage: tally categories delete <path> [--force] [--json]"),
            "{text}"
        );
        assert!(text.contains("--force"), "{text}");
        assert!(text.contains("--json"), "{text}");
    }

    #[test]
    fn render_categorise_shows_both_forms() {
        let text = render_help(HelpTopic::Categorise);
        assert!(
            text.contains("categorise <tx-id> <category-path> [--json]"),
            "{text}"
        );
        assert!(
            text.contains("categorise <tx-id> --clear [--json]"),
            "{text}"
        );
        assert!(text.contains("--json"), "{text}");
    }

    #[test]
    fn render_family_lists_all_leaves() {
        let text = render_help(HelpTopic::Categories);
        for leaf in ["list", "rename", "merge", "delete"] {
            assert!(text.contains(&format!("categories {leaf}")), "{text}");
        }
        assert!(text.contains("--help"), "{text}");
    }

    #[test]
    fn render_global_contains_every_command() {
        let text = render_help(HelpTopic::Global);
        for needle in [
            "(none)",
            "pull",
            "classify",
            "tui",
            "categories list",
            "categories rename",
            "categories merge",
            "categories delete",
            "transactions list",
            "categorise <tx-id> <category-path>",
            "categorise <tx-id> --clear",
            "ai install-claude-skill",
            "--vault PATH",
            "--json",
            "--csv",
            "--limit N",
        ] {
            assert!(
                text.contains(needle),
                "global help missing {needle:?}:\n{text}"
            );
        }
    }

    #[test]
    fn parse_error_points_at_help() {
        let err = parse_command(&s(&["categories", "rename", "A"])).unwrap_err();
        assert!(err.contains("Usage: tally categories rename"), "{err}");
        assert!(err.contains("see: tally categories rename --help"), "{err}");
    }

    // ---------- parser tests ----------

    #[test]
    fn parses_categories_list_formats() {
        assert_eq!(
            parse_command(&s(&["categories", "list"])).unwrap(),
            Command::CategoriesList {
                format: Format::Text
            }
        );
        assert_eq!(
            parse_command(&s(&["categories", "list", "--json"])).unwrap(),
            Command::CategoriesList {
                format: Format::Json
            }
        );
        assert_eq!(
            parse_command(&s(&["categories", "list", "--csv"])).unwrap(),
            Command::CategoriesList {
                format: Format::Csv
            }
        );
    }

    #[test]
    fn parses_category_mutations() {
        assert_eq!(
            parse_command(&s(&["categories", "rename", "Food", "Food/Out"])).unwrap(),
            Command::CategoryRename {
                from: "Food".into(),
                to: "Food/Out".into(),
                json: false,
            }
        );
        assert_eq!(
            parse_command(&s(&["categories", "merge", "A", "B", "--json"])).unwrap(),
            Command::CategoryMerge {
                source: "A".into(),
                target: "B".into(),
                json: true,
            }
        );
        assert_eq!(
            parse_command(&s(&["categories", "delete", "Old"])).unwrap(),
            Command::CategoryDelete {
                path: "Old".into(),
                json: false,
                force: false,
            }
        );
        assert_eq!(
            parse_command(&s(&["categories", "delete", "Old", "--force"])).unwrap(),
            Command::CategoryDelete {
                path: "Old".into(),
                json: false,
                force: true,
            }
        );
    }

    #[test]
    fn delete_csv_flag_errors_as_list_only() {
        let err = parse_command(&s(&["categories", "delete", "Old", "--csv"])).unwrap_err();
        assert!(err.contains("only valid on `list`"), "{err}");
    }

    #[test]
    fn parses_transactions_list_with_query_and_limit() {
        assert_eq!(
            parse_command(&s(&[
                "transactions",
                "list",
                "coffee",
                "OR",
                "tea",
                "--limit",
                "10",
            ]))
            .unwrap(),
            Command::TransactionsList {
                query: "coffee OR tea".into(),
                limit: 10,
                format: Format::Text,
            }
        );
        // Default limit, csv format.
        assert_eq!(
            parse_command(&s(&["transactions", "list", "--csv"])).unwrap(),
            Command::TransactionsList {
                query: String::new(),
                limit: DEFAULT_LIMIT,
                format: Format::Csv,
            }
        );
    }

    #[test]
    fn parses_categorise_set_and_clear() {
        assert_eq!(
            parse_command(&s(&["categorise", "42", "Food/Groceries"])).unwrap(),
            Command::Categorise {
                tx_id: 42,
                path: "Food/Groceries".into(),
                json: false,
            }
        );
        assert_eq!(
            parse_command(&s(&["categorise", "42", "--clear"])).unwrap(),
            Command::Uncategorise {
                tx_id: 42,
                json: false,
            }
        );
        assert_eq!(
            parse_command(&s(&["categorise", "7", "--clear", "--json"])).unwrap(),
            Command::Uncategorise {
                tx_id: 7,
                json: true,
            }
        );
    }

    #[test]
    fn json_and_csv_together_errors() {
        let err = parse_command(&s(&["categories", "list", "--json", "--csv"])).unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn csv_on_non_list_command_errors() {
        let err = parse_command(&s(&["categories", "rename", "A", "B", "--csv"])).unwrap_err();
        assert!(err.contains("only valid on `list`"), "{err}");
        let err = parse_command(&s(&["categorise", "1", "Food", "--csv"])).unwrap_err();
        assert!(err.contains("only valid on `list`"), "{err}");
    }

    #[test]
    fn unknown_command_errors() {
        let err = parse_command(&s(&["frobnicate"])).unwrap_err();
        assert!(err.contains("Unknown command"), "{err}");
        let err = parse_command(&s(&["categories", "bogus"])).unwrap_err();
        assert!(err.contains("Unknown categories subcommand"), "{err}");
    }

    #[test]
    fn invalid_tx_id_errors() {
        let err = parse_command(&s(&["categorise", "notanumber", "Food"])).unwrap_err();
        assert!(err.contains("Invalid transaction id"), "{err}");
    }

    #[test]
    fn missing_positionals_error() {
        assert!(parse_command(&s(&["categories", "rename", "A"])).is_err());
        assert!(parse_command(&s(&["categories", "merge", "A"])).is_err());
        assert!(parse_command(&s(&["categorise"])).is_err());
    }

    #[test]
    fn vault_already_stripped_command_still_parses() {
        // main.rs strips --vault before calling parse_command, so the slice we
        // receive interleaves cleanly. Mimic the post-strip token order.
        assert_eq!(
            parse_command(&s(&["categories", "list", "--json"])).unwrap(),
            Command::CategoriesList {
                format: Format::Json
            }
        );
    }

    // ---------- csv quoting tests ----------

    #[test]
    fn csv_field_quotes_when_needed() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("has,comma"), "\"has,comma\"");
        assert_eq!(csv_field("has\"quote"), "\"has\"\"quote\"");
        assert_eq!(csv_field("line\nbreak"), "\"line\nbreak\"");
    }

    #[test]
    fn csv_row_joins_and_terminates() {
        assert_eq!(csv_row(&["a", "b,c", "d\"e"]), "a,\"b,c\",\"d\"\"e\"\r\n");
    }

    // ---------- ai install tests ----------

    #[test]
    fn install_claude_skill_writes_embedded_skill() {
        let vault = TempDir::new().unwrap();
        run_ai(&s(&["ai", "install-claude-skill"]), vault.path()).unwrap();

        let path = vault.path().join(CLAUDE_SKILL_REL);
        assert_eq!(fs::read_to_string(&path).unwrap(), CLAUDE_SKILL_MD);

        // Re-running overwrites in place (idempotent).
        run_ai(&s(&["ai", "install-claude-skill"]), vault.path()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), CLAUDE_SKILL_MD);
    }

    #[test]
    fn ai_unknown_or_missing_subcommand_errors() {
        let vault = TempDir::new().unwrap();
        assert!(run_ai(&s(&["ai"]), vault.path()).is_err());
        assert!(run_ai(&s(&["ai", "bogus"]), vault.path()).is_err());
        assert!(run_ai(&s(&["ai", "install-claude-skill", "extra"]), vault.path()).is_err());
    }

    // ---------- executor tests ----------

    fn make_executable(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = path;
    }

    fn setup_test_exports() -> TempDir {
        let temp = TempDir::new().unwrap();
        let account_dir = temp.path().join("TestBank").join("Checking");
        fs::create_dir_all(&account_dir).unwrap();
        fs::write(
            account_dir.join("transactions.csv"),
            "Date,Description,Amount,Balance\n2025-01-01,Test,-100,500\n",
        )
        .unwrap();
        let import_script = account_dir.join("import");
        fs::write(
            &import_script,
            r#"#!/usr/bin/env bash
echo '[{"date":"2025-01-01","description":"Test transaction","amount_cents":-10000,"balance_cents":50000}]'
"#,
        )
        .unwrap();
        make_executable(&import_script);
        temp
    }

    fn fixture_store() -> (TempDir, TransactionStore) {
        let temp = setup_test_exports();
        let mut store = TransactionStore::open_in_memory(temp.path()).unwrap();
        store.refresh().unwrap();
        (temp, store)
    }

    fn only_transaction_id(store: &TransactionStore) -> i64 {
        let txs = store
            .query_transactions(&tally::search::ParsedQuery::empty(), None)
            .unwrap();
        assert_eq!(
            txs.len(),
            1,
            "fixture should import exactly one transaction"
        );
        txs[0].id
    }

    fn category(store: &TransactionStore, path: &str) -> Option<Category> {
        store.get_category_by_path(path).unwrap()
    }

    #[test]
    fn rename_executor_changes_path() {
        let (_temp, mut store) = fixture_store();
        let tx_id = only_transaction_id(&store);
        let cat_id = store.get_or_create_category("Food").unwrap();
        store
            .set_category(tx_id, cat_id, CategorySource::Manual, true, None)
            .unwrap();

        run(
            Command::CategoryRename {
                from: "Food".into(),
                to: "Groceries".into(),
                json: false,
            },
            &mut store,
        )
        .unwrap();

        assert!(category(&store, "Food").is_none());
        assert_eq!(category(&store, "Groceries").map(|c| c.id), Some(cat_id));
    }

    #[test]
    fn rename_to_existing_path_errors_with_merge_hint() {
        let (_temp, mut store) = fixture_store();
        store.get_or_create_category("Food").unwrap();
        store.get_or_create_category("Groceries").unwrap();

        let err = run(
            Command::CategoryRename {
                from: "Food".into(),
                to: "Groceries".into(),
                json: false,
            },
            &mut store,
        )
        .unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert!(err.contains("tally categories merge"), "{err}");
    }

    #[test]
    fn merge_executor_moves_enrichments_and_removes_source() {
        let (_temp, mut store) = fixture_store();
        let tx_id = only_transaction_id(&store);
        let source = store.get_or_create_category("Food").unwrap();
        let target = store.get_or_create_category("Groceries").unwrap();
        store
            .set_category(tx_id, source, CategorySource::Manual, true, None)
            .unwrap();

        run(
            Command::CategoryMerge {
                source: "Food".into(),
                target: "Groceries".into(),
                json: false,
            },
            &mut store,
        )
        .unwrap();

        assert!(category(&store, "Food").is_none());
        assert_eq!(
            store.get_transaction_category(tx_id).unwrap().map(|c| c.id),
            Some(target)
        );
    }

    #[test]
    fn delete_executor_uncategorises_rows() {
        let (_temp, mut store) = fixture_store();
        let tx_id = only_transaction_id(&store);
        let cat_id = store.get_or_create_category("Food").unwrap();
        store
            .set_category(tx_id, cat_id, CategorySource::Manual, true, None)
            .unwrap();

        run(
            Command::CategoryDelete {
                path: "Food".into(),
                json: false,
                force: false,
            },
            &mut store,
        )
        .unwrap();

        assert!(category(&store, "Food").is_none());
        assert!(store.get_transaction_category(tx_id).unwrap().is_none());
    }

    #[test]
    fn delete_blocked_when_filter_references_category() {
        let (_temp, mut store) = fixture_store();
        let cat_id = store.get_or_create_category("Food").unwrap();
        let filter_id = store.create_filter("groceries", "Test").unwrap();
        store.set_filter_category(filter_id, Some(cat_id)).unwrap();

        let err = run(
            Command::CategoryDelete {
                path: "Food".into(),
                json: false,
                force: false,
            },
            &mut store,
        )
        .unwrap_err();
        assert!(err.contains("groceries"), "{err}");
        assert!(err.contains("--force"), "{err}");
        // The category still exists; nothing was deleted.
        assert!(category(&store, "Food").is_some());
    }

    #[test]
    fn delete_force_clears_referencing_filter() {
        let (_temp, mut store) = fixture_store();
        let cat_id = store.get_or_create_category("Food").unwrap();
        let filter_id = store.create_filter("groceries", "Test").unwrap();
        store.set_filter_category(filter_id, Some(cat_id)).unwrap();

        run(
            Command::CategoryDelete {
                path: "Food".into(),
                json: false,
                force: true,
            },
            &mut store,
        )
        .unwrap();

        assert!(category(&store, "Food").is_none());
        let filter = store
            .list_filters()
            .unwrap()
            .into_iter()
            .find(|f| f.id == filter_id)
            .unwrap();
        assert_eq!(filter.category_id, None);
    }

    #[test]
    fn merge_executor_repoints_referencing_filter() {
        let (_temp, mut store) = fixture_store();
        let source = store.get_or_create_category("Food").unwrap();
        let target = store.get_or_create_category("Groceries").unwrap();
        let filter_id = store.create_filter("food", "Test").unwrap();
        store.set_filter_category(filter_id, Some(source)).unwrap();

        run(
            Command::CategoryMerge {
                source: "Food".into(),
                target: "Groceries".into(),
                json: false,
            },
            &mut store,
        )
        .unwrap();

        let filter = store
            .list_filters()
            .unwrap()
            .into_iter()
            .find(|f| f.id == filter_id)
            .unwrap();
        assert_eq!(filter.category_id, Some(target));
    }

    #[test]
    fn categorise_set_then_clear() {
        let (_temp, mut store) = fixture_store();
        let tx_id = only_transaction_id(&store);

        run(
            Command::Categorise {
                tx_id,
                path: "Food/Groceries".into(),
                json: false,
            },
            &mut store,
        )
        .unwrap();
        assert_eq!(
            store
                .get_transaction_category(tx_id)
                .unwrap()
                .map(|c| c.path),
            Some("Food/Groceries".to_string())
        );

        run(Command::Uncategorise { tx_id, json: false }, &mut store).unwrap();
        assert!(store.get_transaction_category(tx_id).unwrap().is_none());
    }

    #[test]
    fn categorise_missing_transaction_errors() {
        let (_temp, mut store) = fixture_store();
        let err = run(
            Command::Categorise {
                tx_id: 999_999,
                path: "Food".into(),
                json: false,
            },
            &mut store,
        )
        .unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn rename_missing_source_errors() {
        let (_temp, mut store) = fixture_store();
        let err = run(
            Command::CategoryRename {
                from: "Nope".into(),
                to: "Other".into(),
                json: false,
            },
            &mut store,
        )
        .unwrap_err();
        assert!(err.contains("not found"), "{err}");
    }
}
