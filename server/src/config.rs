use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{collections::HashSet, path::PathBuf};

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub bind: String,
    pub origin: String,
    pub secure_cookie: bool,
    pub data_dir: PathBuf,
    pub web_dir: PathBuf,
    pub allowed_hosts: HashSet<String>,
    pub result_hosts: HashSet<String>,
    pub concurrency: usize,
    pub cipher: Aes256Gcm,
}
impl Config {
    pub fn load() -> Result<Self> {
        let read =
            |name: &str, default: &str| std::env::var(name).unwrap_or_else(|_| default.into());
        let encoded =
            std::fs::read_to_string(read("NEWPHOTO_MASTER_KEY_FILE", "secrets/master.key"))
                .context("read master key; run newphoto key generate first")?;
        let key = STANDARD
            .decode(encoded.trim())
            .context("master key must be base64")?;
        if key.len() != 32 {
            bail!("master key must contain 32 bytes");
        }
        let origin = read("NEWPHOTO_PUBLIC_ORIGIN", "http://127.0.0.1:8080");
        let secure_cookie = read("NEWPHOTO_COOKIE_SECURE", "true") == "true";
        let parsed = url::Url::parse(&origin)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !parsed.username().is_empty()
        {
            bail!("public origin must be an origin without a path");
        }
        if !secure_cookie && !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1")) {
            bail!("insecure cookies require explicit loopback development");
        }
        let host_set = |name: &str| {
            read(name, "")
                .split(',')
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.trim().to_lowercase())
                .collect()
        };
        Ok(Self {
            database_url: std::env::var("DATABASE_URL").context("DATABASE_URL is required")?,
            bind: read("NEWPHOTO_BIND", "127.0.0.1:8080"),
            origin: origin.trim_end_matches('/').into(),
            secure_cookie,
            data_dir: read("NEWPHOTO_DATA_DIR", "data").into(),
            web_dir: read("NEWPHOTO_WEB_DIR", "web/dist").into(),
            allowed_hosts: host_set("NEWPHOTO_ALLOWED_UPSTREAMS"),
            result_hosts: host_set("NEWPHOTO_RESULT_HOSTS"),
            concurrency: read("NEWPHOTO_TASK_CONCURRENCY", "2")
                .parse::<usize>()?
                .clamp(1, 8),
            cipher: Aes256Gcm::new_from_slice(&key).map_err(|_| anyhow::anyhow!("invalid key"))?,
        })
    }
    pub fn encrypt(&self, text: &str) -> Result<String> {
        let nonce_bytes = rand::random::<[u8; 12]>();
        let encrypted = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), text.as_bytes())
            .map_err(|_| anyhow::anyhow!("encryption failed"))?;
        Ok(STANDARD.encode([nonce_bytes.as_slice(), &encrypted].concat()))
    }
    pub fn decrypt(&self, encoded: &str) -> Result<String> {
        let bytes = STANDARD.decode(encoded)?;
        if bytes.len() < 28 {
            bail!("invalid encrypted secret");
        }
        let plain = self
            .cipher
            .decrypt(Nonce::from_slice(&bytes[..12]), &bytes[12..])
            .map_err(|_| anyhow::anyhow!("decryption failed"))?;
        Ok(String::from_utf8(plain)?)
    }
}
