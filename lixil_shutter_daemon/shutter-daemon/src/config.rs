use std::fs;

use anyhow::{bail, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct ShutterConfig {
    pub id: String,
    pub name: String,
    pub mac: String,
}

#[derive(Debug, Deserialize)]
pub struct AddonConfig {
    #[serde(default = "default_log_level")]
    pub log_level: String,
    pub shutters: Vec<ShutterConfig>,
    // Substring to match against each adapter's id (e.g. "hci1") -- picks a
    // dedicated USB BT dongle instead
    // of the host's onboard adapter, which HA Core's own `bluetooth`
    // integration already owns (confirmed: sharing it with another BlueZ
    // client aborts every connection attempt). Empty/absent = pick whichever
    // adapter was enumerated last, which is usually the dongle since the
    // onboard adapter is initialized first at boot -- but that's a heuristic,
    // not a guarantee; set this explicitly once you've seen the daemon's own
    // adapter-enumeration log line listing what's available.
    #[serde(default)]
    pub bluetooth_adapter: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

impl AddonConfig {
    pub fn load(path: &str) -> Result<Self> {
        let raw = fs::read_to_string(path)?;
        let mut cfg: Self = serde_json::from_str(&raw)?;
        // The id becomes part of every MQTT topic, the HA discovery object_id
        // and unique_id. HA rejects discovery object_ids outside
        // [a-zA-Z0-9_-], and the add-on options editor doesn't enforce the
        // web UI's valid_id() rule, so normalize here (falling back to the
        // name when id is blank) and refuse duplicates after normalization.
        let mut seen = std::collections::HashSet::new();
        for s in &mut cfg.shutters {
            let src = if s.id.trim().is_empty() { &s.name } else { &s.id };
            let slug = slugify(src);
            if slug.is_empty() {
                bail!("shutter '{}' has no usable id", s.name);
            }
            if !seen.insert(slug.clone()) {
                bail!("duplicate shutter id '{slug}' (after normalizing '{}')", s.id);
            }
            s.id = slug;
        }
        Ok(cfg)
    }
}

pub fn slugify(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

pub struct MqttConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl MqttConfig {
    pub fn from_env() -> Self {
        let port = std::env::var("MQTT_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(1883);
        Self {
            host: std::env::var("MQTT_HOST").unwrap_or_else(|_| "core-mosquitto".to_string()),
            port,
            username: std::env::var("MQTT_USER").ok().filter(|s| !s.is_empty()),
            password: std::env::var("MQTT_PASSWORD").ok().filter(|s| !s.is_empty()),
        }
    }
}
