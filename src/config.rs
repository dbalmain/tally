//! Vault-level configuration loaded from `<vault>/tally.toml`.

use std::path::Path;

use chrono::{Local, NaiveDate, Weekday};
use serde::Deserialize;

use crate::search::SearchOptions;
use crate::{Error, Result};

const CONFIG_FILE: &str = "tally.toml";
const DEFAULT_WEEK_START: Weekday = Weekday::Mon;
const DEFAULT_YEAR_END: (u32, u32) = (6, 30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub week_start: Weekday,
    pub financial_year_end: (u32, u32),
}

impl Config {
    pub fn load(vault_root: &Path) -> Result<Self> {
        let path = vault_root.join(CONFIG_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(err.into()),
        };

        let raw: RawConfig = toml::from_str(&text).map_err(|source| Error::ConfigParse {
            path: path.clone(),
            source,
        })?;
        Self::from_raw(raw, &path)
    }

    fn from_raw(raw: RawConfig, path: &Path) -> Result<Self> {
        let week_start = raw
            .dates
            .and_then(|dates| dates.week_start)
            .map(|value| parse_weekday(&value, path))
            .transpose()?
            .unwrap_or(DEFAULT_WEEK_START);

        let financial_year_end = raw
            .tax
            .and_then(|tax| tax.year_end)
            .map(|value| parse_year_end(&value, path))
            .transpose()?
            .unwrap_or(DEFAULT_YEAR_END);

        Ok(Self {
            week_start,
            financial_year_end,
        })
    }

    pub fn search_options(&self) -> SearchOptions {
        self.search_options_for(Local::now().date_naive())
    }

    pub fn search_options_for(&self, today: NaiveDate) -> SearchOptions {
        SearchOptions::new(today, self.week_start, self.financial_year_end)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            week_start: DEFAULT_WEEK_START,
            financial_year_end: DEFAULT_YEAR_END,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    dates: Option<RawDates>,
    tax: Option<RawTax>,
}

#[derive(Debug, Deserialize)]
struct RawDates {
    week_start: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawTax {
    year_end: Option<String>,
}

fn parse_weekday(value: &str, path: &Path) -> Result<Weekday> {
    match value.to_ascii_lowercase().as_str() {
        "monday" => Ok(Weekday::Mon),
        "tuesday" => Ok(Weekday::Tue),
        "wednesday" => Ok(Weekday::Wed),
        "thursday" => Ok(Weekday::Thu),
        "friday" => Ok(Weekday::Fri),
        "saturday" => Ok(Weekday::Sat),
        "sunday" => Ok(Weekday::Sun),
        _ => Err(Error::ConfigValue {
            path: path.to_path_buf(),
            message: format!("dates.week_start must be a full weekday name, got {value:?}"),
        }),
    }
}

fn parse_year_end(value: &str, path: &Path) -> Result<(u32, u32)> {
    let Some((month, day)) = value.split_once('-') else {
        return Err(invalid_year_end(value, path));
    };
    if month.len() != 2 || day.len() != 2 {
        return Err(invalid_year_end(value, path));
    }

    let Ok(month) = month.parse::<u32>() else {
        return Err(invalid_year_end(value, path));
    };
    let Ok(day) = day.parse::<u32>() else {
        return Err(invalid_year_end(value, path));
    };

    if NaiveDate::from_ymd_opt(2023, month, day).is_none() {
        return Err(invalid_year_end(value, path));
    }

    Ok((month, day))
}

fn invalid_year_end(value: &str, path: &Path) -> Error {
    Error::ConfigValue {
        path: path.to_path_buf(),
        message: format!("tax.year_end must be MM-DD and valid every year, got {value:?}"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn missing_file_uses_defaults() {
        let dir = TempDir::new().unwrap();

        let config = Config::load(dir.path()).unwrap();

        assert_eq!(config, Config::default());
    }

    #[test]
    fn loads_known_fields_and_ignores_unknowns() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(CONFIG_FILE),
            r#"
                ignored = true

                [dates]
                week_start = "Sunday"
                extra = "ignored"

                [tax]
                year_end = "03-31"
                lodge_by = "2027-10-31"
            "#,
        )
        .unwrap();

        let config = Config::load(dir.path()).unwrap();

        assert_eq!(config.week_start, Weekday::Sun);
        assert_eq!(config.financial_year_end, (3, 31));
    }

    #[test]
    fn defaults_missing_sections_and_fields() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(CONFIG_FILE), "[dates]\n").unwrap();

        let config = Config::load(dir.path()).unwrap();

        assert_eq!(config, Config::default());
    }

    #[test]
    fn malformed_file_is_an_error() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join(CONFIG_FILE), "[dates\n").unwrap();

        assert!(matches!(
            Config::load(dir.path()),
            Err(Error::ConfigParse { .. })
        ));
    }

    #[test]
    fn invalid_weekday_is_an_error() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(CONFIG_FILE),
            "[dates]\nweek_start = \"moonday\"\n",
        )
        .unwrap();

        assert!(matches!(
            Config::load(dir.path()),
            Err(Error::ConfigValue { .. })
        ));
    }

    #[test]
    fn invalid_year_end_is_an_error() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join(CONFIG_FILE),
            "[tax]\nyear_end = \"02-29\"\n",
        )
        .unwrap();

        assert!(matches!(
            Config::load(dir.path()),
            Err(Error::ConfigValue { .. })
        ));
    }
}
