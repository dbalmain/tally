use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, RawTransaction, Result};

fn find_script(exports_dir: &Path, bank: &str, account: &str, name: &str) -> Option<PathBuf> {
    let account_script = exports_dir.join(bank).join(account).join(name);
    if account_script.exists() {
        return Some(account_script);
    }

    let bank_script = exports_dir.join(bank).join(name);
    if bank_script.exists() {
        return Some(bank_script);
    }

    None
}

pub(crate) fn find_import_script(exports_dir: &Path, bank: &str, account: &str) -> Option<PathBuf> {
    find_script(exports_dir, bank, account, "import")
}

fn run_script(
    script_path: &Path,
    arg: Option<&Path>,
    cwd: &Path,
    kind: &str,
) -> Result<Vec<RawTransaction>> {
    let abs_script = std::fs::canonicalize(script_path)?;
    let abs_arg = arg.map(std::fs::canonicalize).transpose()?;
    let abs_cwd = std::fs::canonicalize(cwd)?;

    let mut command = Command::new(&abs_script);
    if let Some(arg) = &abs_arg {
        command.arg(arg);
    }

    let output = command.current_dir(abs_cwd).output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::ImportFailed(format!(
            "{} {} failed with status {}: {}",
            kind,
            script_path.display(),
            output.status,
            stderr
        )));
    }

    let transactions: Vec<RawTransaction> = serde_json::from_slice(&output.stdout)?;
    Ok(transactions)
}

pub(crate) fn run_import_script(
    script_path: &Path,
    csv_file: &Path,
) -> Result<Vec<RawTransaction>> {
    let cwd = csv_file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    run_script(script_path, Some(csv_file), cwd, "Script")
}

/// Locate an executable `pull` script for an account.
///
/// A pull script fetches transactions from an external source (e.g. an API)
/// rather than parsing a dropped CSV. Account-level scripts override
/// bank-level ones, mirroring [`find_import_script`].
pub(crate) fn find_pull_script(exports_dir: &Path, bank: &str, account: &str) -> Option<PathBuf> {
    find_script(exports_dir, bank, account, "pull")
}

/// Run a `pull` script with no CSV argument, returning the transactions it
/// emits as JSON on stdout.
///
/// The script runs with its account directory as the working directory so it
/// can read/write any per-account state (e.g. an incremental-pull log) there.
pub(crate) fn run_pull_script(
    script_path: &Path,
    account_dir: &Path,
) -> Result<Vec<RawTransaction>> {
    run_script(script_path, None, account_dir, "Pull script")
}

pub(crate) fn compute_hash(
    date: &str,
    description: &str,
    amount_cents: i64,
    balance_cents: i64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(date.as_bytes());
    hasher.update(b"|");
    hasher.update(description.as_bytes());
    hasher.update(b"|");
    hasher.update(amount_cents.to_le_bytes());
    hasher.update(b"|");
    hasher.update(balance_cents.to_le_bytes());
    hex::encode(hasher.finalize())
}

pub(crate) fn find_csv_files(account_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(account_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
        {
            files.push(path);
        }
    }
    Ok(files)
}

pub(crate) fn hash_file(path: &Path) -> Result<String> {
    use std::io::{BufReader, Read};
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use tempfile::TempDir;

    fn make_executable(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }

        #[cfg(not(unix))]
        let _ = path;
    }

    fn write_script(path: &Path, contents: impl AsRef<str>) {
        fs::write(path, contents.as_ref()).unwrap();
        make_executable(path);
    }

    fn import_failed_message(result: Result<Vec<RawTransaction>>) -> String {
        match result {
            Err(Error::ImportFailed(message)) => message,
            other => panic!("expected import failure, got {other:?}"),
        }
    }

    #[test]
    fn test_compute_hash() {
        let hash1 = compute_hash("2025-01-01", "Test transaction", -10000, 50000);
        let hash2 = compute_hash("2025-01-01", "Test transaction", -10000, 50000);
        let hash3 = compute_hash("2025-01-02", "Test transaction", -10000, 50000);

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn find_script_prefers_account_level_then_bank_level() {
        let temp = TempDir::new().unwrap();
        let bank_dir = temp.path().join("Bank");
        let account_dir = bank_dir.join("Account");
        fs::create_dir_all(&account_dir).unwrap();

        let bank_import = bank_dir.join("import");
        let account_import = account_dir.join("import");
        let bank_pull = bank_dir.join("pull");
        let account_pull = account_dir.join("pull");
        fs::write(&bank_import, "").unwrap();
        fs::write(&account_import, "").unwrap();
        fs::write(&bank_pull, "").unwrap();
        fs::write(&account_pull, "").unwrap();

        assert_eq!(
            find_import_script(temp.path(), "Bank", "Account"),
            Some(account_import.clone())
        );
        assert_eq!(
            find_pull_script(temp.path(), "Bank", "Account"),
            Some(account_pull.clone())
        );

        fs::remove_file(account_import).unwrap();
        fs::remove_file(account_pull).unwrap();

        assert_eq!(
            find_import_script(temp.path(), "Bank", "Account"),
            Some(bank_import)
        );
        assert_eq!(
            find_pull_script(temp.path(), "Bank", "Account"),
            Some(bank_pull)
        );
    }

    #[test]
    fn run_import_and_pull_scripts_parse_stdout_with_expected_args_and_cwd() {
        let temp = TempDir::new().unwrap();
        let account_dir = temp.path().join("Bank").join("Account");
        fs::create_dir_all(&account_dir).unwrap();
        let csv_file = account_dir.join("transactions.csv");
        fs::write(&csv_file, "date,description,amount,balance\n").unwrap();

        let expected_cwd = account_dir.canonicalize().unwrap();
        let expected_csv = csv_file.canonicalize().unwrap();

        let import_script = account_dir.join("import");
        write_script(
            &import_script,
            format!(
                r#"#!/usr/bin/env bash
if [[ "$PWD" != "{}" ]]; then echo "cwd=$PWD" >&2; exit 2; fi
if [[ "$1" != "{}" ]]; then echo "arg=$1" >&2; exit 3; fi
echo '[{{"date":"2025-01-01","description":"import ok","amount_cents":-100,"balance_cents":500}}]'
"#,
                expected_cwd.display(),
                expected_csv.display()
            ),
        );

        let pull_script = account_dir.join("pull");
        write_script(
            &pull_script,
            format!(
                r#"#!/usr/bin/env bash
if [[ "$PWD" != "{}" ]]; then echo "cwd=$PWD" >&2; exit 2; fi
if [[ "$#" != "0" ]]; then echo "args=$#" >&2; exit 3; fi
echo '[{{"date":"2025-01-02","description":"pull ok","amount_cents":-200,"balance_cents":300}}]'
"#,
                expected_cwd.display()
            ),
        );

        let imported = run_import_script(&import_script, &csv_file).unwrap();
        let pulled = run_pull_script(&pull_script, &account_dir).unwrap();

        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].description, "import ok");
        assert_eq!(pulled.len(), 1);
        assert_eq!(pulled[0].description, "pull ok");
    }

    #[test]
    fn run_scripts_preserve_failure_prefixes() {
        let temp = TempDir::new().unwrap();
        let account_dir = temp.path().join("Bank").join("Account");
        fs::create_dir_all(&account_dir).unwrap();
        let csv_file = account_dir.join("transactions.csv");
        fs::write(&csv_file, "date,description,amount,balance\n").unwrap();

        let import_script = account_dir.join("import");
        let pull_script = account_dir.join("pull");
        write_script(
            &import_script,
            "#!/usr/bin/env bash\necho import failed >&2\nexit 7\n",
        );
        write_script(
            &pull_script,
            "#!/usr/bin/env bash\necho pull failed >&2\nexit 8\n",
        );

        let import_message = import_failed_message(run_import_script(&import_script, &csv_file));
        let pull_message = import_failed_message(run_pull_script(&pull_script, &account_dir));

        assert!(import_message.starts_with(&format!(
            "Script {} failed with status ",
            import_script.display()
        )));
        assert!(import_message.ends_with(": import failed\n"));
        assert!(pull_message.starts_with(&format!(
            "Pull script {} failed with status ",
            pull_script.display()
        )));
        assert!(pull_message.ends_with(": pull failed\n"));
    }
}
