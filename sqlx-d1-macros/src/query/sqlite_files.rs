use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

pub type HmacSha256 = Hmac<Sha256>;

pub fn durable_object_namespace_id_from_database_id(unique_key: &str, database_id: &str) -> String {
    // SHA-256 hash of unique_key to create the HMAC key
    let key = {
        use sha2::Digest;
        sha2::Sha256::digest(unique_key.as_bytes())
    };

    // First HMAC: key over database_id, take first 16 bytes
    let database_id_hmac = {
        let mut mac = HmacSha256::new_from_slice(&key).expect("HMAC can take key of any size");
        mac.update(database_id.as_bytes());
        let result = mac.finalize().into_bytes();
        result[..16].to_vec()
    };

    // Second HMAC: key over database_id_hmac, take first 16 bytes
    let hmac = {
        let mut mac = HmacSha256::new_from_slice(&key).expect("HMAC can take key of any size");
        mac.update(&database_id_hmac);
        let result = mac.finalize().into_bytes();
        result[..16].to_vec()
    };

    // Concatenate and hex-encode
    let combined: Vec<u8> = database_id_hmac.into_iter().chain(hmac).collect();
    hex::encode(combined)
}

/// Compute the expected SQLite filename for a given database_id.
/// Uses the hardcoded unique key "miniflare-D1DatabaseObject".
pub fn expected_sqlite_filename(database_id: &str) -> String {
    durable_object_namespace_id_from_database_id("miniflare-D1DatabaseObject", database_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::wrangler_config::extract_database_ids;

    #[test]
    fn test_staging_db() {
        let hash = durable_object_namespace_id_from_database_id(
            "miniflare-D1DatabaseObject",
            "b8e63bb5-1234-49f6-abcd-a5bd4d724e69",
        );
        assert_eq!(hash, "90631cd2742181c8321b3ef618ce3aa8712caf50b14959037244fdeb561d8d1a");
    }

    #[test]
    fn test_production_db() {
        let hash = durable_object_namespace_id_from_database_id(
            "miniflare-D1DatabaseObject",
            "eb56671d-7425-1234-9ff2-abcda13d7c11",
        );
        assert_eq!(hash, "5e477bcef7eeb0569f4e2e3d67ce1094341ab7ed685dda47f2afb66f479af046");
    }

    #[test]
    fn test_parse_no_env_jsonc() {
        let content = include_str!("../../assets/no_env.jsonc");
        let config = parse_wrangler_config_from_content(content, "jsonc").unwrap();
        let ids = extract_database_ids(&config);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "12345678-1234-abcd-zyxw-012345678910");
    }

    #[test]
    fn test_parse_envs_jsonc() {
        let content = include_str!("../../assets/envs.jsonc");
        let config = parse_wrangler_config_from_content(content, "jsonc").unwrap();
        let ids = extract_database_ids(&config);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"12345678-abcd-1234-zyxw-012345678910".to_string()));
        assert!(ids.contains(&"12345678-1234-zyxw-abcd-012345678910".to_string()));
    }

    #[test]
    fn test_parse_no_env_toml() {
        let content = include_str!("../../assets/no_env.toml");
        let config = parse_wrangler_config_from_content(content, "toml").unwrap();
        let ids = extract_database_ids(&config);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "12345678-1234-abcd-zyxw-012345678910");
    }

    #[test]
    fn test_parse_envs_toml() {
        let content = include_str!("../../assets/envs.toml");
        let config = parse_wrangler_config_from_content(content, "toml").unwrap();
        let ids = extract_database_ids(&config);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"12345678-abcd-1234-zyxw-012345678910".to_string()));
        assert!(ids.contains(&"12345678-1234-zyxw-abcd-012345678910".to_string()));
    }

    #[test]
    fn test_parse_no_env_comments_jsonc() {
        let content = include_str!("../../assets/no_env_comments.jsonc");
        let config = parse_wrangler_config_from_content(content, "jsonc").unwrap();
        let ids = extract_database_ids(&config);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "12345678-1234-abcd-zyxw-012345678910");
    }

    // Helper function to parse config from content (mimics parse_wrangler_config but for strings)
    fn parse_wrangler_config_from_content(content: &str, format: &str) -> Result<crate::query::wrangler_config::WranglerConfig, crate::query::wrangler_config::WranglerConfigError> {
        match format {
            "jsonc" => crate::query::wrangler_config::parse_jsonc(content),
            "toml" => crate::query::wrangler_config::parse_toml(content),
            _ => Err(crate::query::wrangler_config::WranglerConfigError::InvalidJsonc("Unsupported format".to_string())),
        }
    }
}
