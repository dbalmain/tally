use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

use crate::{Error, RawTransaction, Result};

pub(crate) fn find_import_script(exports_dir: &Path, bank: &str, account: &str) -> Option<std::path::PathBuf> {
    let account_script = exports_dir.join(bank).join(account).join("import");
    if account_script.exists() {
        return Some(account_script);
    }

    let bank_script = exports_dir.join(bank).join("import");
    if bank_script.exists() {
        return Some(bank_script);
    }

    None
}

pub(crate) fn run_import_script(
    script_path: &Path,
    csv_file: &Path,
) -> Result<Vec<RawTransaction>> {
    let abs_script = std::fs::canonicalize(script_path)?;
    let abs_csv = std::fs::canonicalize(csv_file)?;
    
    let output = Command::new(&abs_script)
        .arg(&abs_csv)
        .current_dir(abs_csv.parent().unwrap_or(Path::new(".")))
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::ImportFailed(format!(
            "Script {} failed with status {}: {}",
            script_path.display(),
            output.status,
            stderr
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let transactions: Vec<RawTransaction> = serde_json::from_str(&stdout)?;
    Ok(transactions)
}

pub(crate) fn compute_hash(date: &str, description: &str, amount_cents: i64, balance_cents: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(date.as_bytes());
    hasher.update(b"|");
    hasher.update(description.as_bytes());
    hasher.update(b"|");
    hasher.update(amount_cents.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(balance_cents.to_string().as_bytes());
    hex::encode(hasher.finalize())
}

pub(crate) fn find_csv_files(account_dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(account_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("csv")) {
            files.push(path);
        }
    }
    Ok(files)
}

pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let contents = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&contents);
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_hash() {
        let hash1 = compute_hash("2025-01-01", "Test transaction", -10000, 50000);
        let hash2 = compute_hash("2025-01-01", "Test transaction", -10000, 50000);
        let hash3 = compute_hash("2025-01-02", "Test transaction", -10000, 50000);

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
        assert_eq!(hash1.len(), 64);
    }
}
