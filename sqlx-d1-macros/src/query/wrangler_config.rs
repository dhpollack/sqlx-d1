use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use json_comments::StripComments;

#[derive(Debug, thiserror::Error)]
pub enum WranglerConfigError {
    #[error("Config file not found")]
    NotFound,
    #[error("Invalid JSONC: {0}")]
    InvalidJsonc(String),
    #[error("Invalid TOML: {0}")]
    InvalidToml(String),
    #[error("No d1_databases found in config")]
    NoDatabases,
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

impl From<WranglerConfigError> for io::Error {
    fn from(err: WranglerConfigError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, err.to_string())
    }
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
pub struct D1Database {
    pub binding: String,
    pub database_name: String,
    pub database_id: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct Environment {
    pub d1_databases: Option<Vec<D1Database>>,
}

#[derive(Debug, serde::Deserialize)]
pub struct WranglerConfig {
    pub d1_databases: Option<Vec<D1Database>>,
    pub envs: Option<HashMap<String, Environment>>,
}

/// Search for wrangler.jsonc or wrangler.toml starting from the given directory.
/// Searches up the directory tree until finding a config file or reaching root.
pub fn find_wrangler_config(start_dir: &Path) -> Result<Option<PathBuf>, io::Error> {
    let mut current = start_dir.to_path_buf();

    loop {
        // Check for wrangler.jsonc first (preferred)
        let jsonc_path = current.join("wrangler.jsonc");
        if jsonc_path.exists() && jsonc_path.is_file() {
            return Ok(Some(jsonc_path));
        }

        // Check for wrangler.toml
        let toml_path = current.join("wrangler.toml");
        if toml_path.exists() && toml_path.is_file() {
            return Ok(Some(toml_path));
        }

        // Move up to parent directory
        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            // Reached root without finding config
            break;
        }
    }

    Ok(None)
}

/// Parse a wrangler config file (JSONC or TOML)
pub fn parse_wrangler_config(path: &Path) -> Result<WranglerConfig, WranglerConfigError> {
    let content = fs::read_to_string(path)?;

    match path.extension().and_then(|ext| ext.to_str()) {
        Some("jsonc") => parse_jsonc(&content),
        Some("toml") => parse_toml(&content),
        _ => Err(WranglerConfigError::InvalidJsonc(
            "Unsupported file extension. Expected .jsonc or .toml".to_string(),
        )),
    }
}

pub(crate) fn parse_jsonc(content: &str) -> Result<WranglerConfig, WranglerConfigError> {
    // Use json_comments crate to strip comments before parsing
    let reader = StripComments::new(content.as_bytes());
    serde_json::from_reader(reader)
        .map_err(|e| WranglerConfigError::InvalidJsonc(e.to_string()))
}

pub(crate) fn parse_toml(content: &str) -> Result<WranglerConfig, WranglerConfigError> {
    toml::from_str(content)
        .map_err(|e| WranglerConfigError::InvalidToml(e.to_string()))
}

/// Extract all database_id values from a config
/// Includes IDs from root d1_databases and all environments
pub fn extract_database_ids(config: &WranglerConfig) -> Vec<String> {
    let mut ids = Vec::new();

    // Add from root d1_databases
    if let Some(dbs) = &config.d1_databases {
        ids.extend(dbs.iter().map(|db| db.database_id.clone()));
    }

    // Add from environments
    if let Some(envs) = &config.envs {
        for env in envs.values() {
            if let Some(dbs) = &env.d1_databases {
                ids.extend(dbs.iter().map(|db| db.database_id.clone()));
            }
        }
    }

    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_database_ids_no_env() {
        let config = WranglerConfig {
            d1_databases: Some(vec![
                D1Database {
                    binding: "test".to_string(),
                    database_name: "test".to_string(),
                    database_id: "12345678-1234-abcd-zyxw-012345678910".to_string(),
                },
            ]),
            envs: None,
        };

        let ids = extract_database_ids(&config);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "12345678-1234-abcd-zyxw-012345678910");
    }

    #[test]
    fn test_extract_database_ids_with_envs() {
        let config = WranglerConfig {
            d1_databases: Some(vec![
                D1Database {
                    binding: "root".to_string(),
                    database_name: "root".to_string(),
                    database_id: "root-id".to_string(),
                },
            ]),
            envs: Some({
                let mut map = HashMap::new();
                map.insert(
                    "staging".to_string(),
                    Environment {
                        d1_databases: Some(vec![
                            D1Database {
                                binding: "staging".to_string(),
                                database_name: "staging".to_string(),
                                database_id: "staging-id".to_string(),
                            },
                        ]),
                    },
                );
                map.insert(
                    "production".to_string(),
                    Environment {
                        d1_databases: Some(vec![
                            D1Database {
                                binding: "production".to_string(),
                                database_name: "production".to_string(),
                                database_id: "production-id".to_string(),
                            },
                        ]),
                    },
                );
                map
            }),
        };

        let ids = extract_database_ids(&config);
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&"root-id".to_string()));
        assert!(ids.contains(&"staging-id".to_string()));
        assert!(ids.contains(&"production-id".to_string()));
    }

    #[test]
    fn test_parse_jsonc_no_env() {
        let content = r#"
        {
          "d1_databases": [
            {
              "binding": "noenv_d1_tutorial",
              "database_name": "noenv-d1-tutorial",
              "database_id": "12345678-1234-abcd-zyxw-012345678910"
            }
          ]
        }
        "#;

        let config = parse_jsonc(content).unwrap();
        assert!(config.d1_databases.is_some());
        assert_eq!(config.d1_databases.as_ref().unwrap().len(), 1);
        assert_eq!(config.d1_databases.as_ref().unwrap()[0].database_id, "12345678-1234-abcd-zyxw-012345678910");
    }

    #[test]
    fn test_parse_jsonc_with_comments() {
        let content = r#"
        // slash comments
        {
          # bash style comment
          "d1_databases": [
            {
              "binding": "noenv_d1_tutorial",
              "database_name": "noenv-d1-tutorial", # end of line comment
              "database_id": "12345678-1234-abcd-zyxw-012345678910"
            }
          ]
        }
        "#;

        let config = parse_jsonc(content).unwrap();
        assert!(config.d1_databases.is_some());
        assert_eq!(config.d1_databases.as_ref().unwrap().len(), 1);
        assert_eq!(config.d1_databases.as_ref().unwrap()[0].database_id, "12345678-1234-abcd-zyxw-012345678910");
    }
}