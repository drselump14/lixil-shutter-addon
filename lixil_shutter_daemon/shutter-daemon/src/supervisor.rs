use anyhow::{anyhow, Result};
use serde_json::{json, Value};

const SUPERVISOR_API: &str = "http://supervisor";

/// Every add-on gets this env var for free, scoped to its own `/addons/self`
/// endpoints -- no extra `hassio_api` permission needed for what this module
/// does (read + write our own options, restart ourselves).
fn token() -> Result<String> {
    std::env::var("SUPERVISOR_TOKEN").map_err(|_| anyhow!("SUPERVISOR_TOKEN not set"))
}

async fn current_options() -> Result<Value> {
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{SUPERVISOR_API}/addons/self/info"))
        .bearer_auth(token()?)
        .send()
        .await?
        .error_for_status()?;
    let body: Value = resp.json().await?;
    body.get("data")
        .and_then(|d| d.get("options"))
        .cloned()
        .ok_or_else(|| anyhow!("unexpected /addons/self/info response shape"))
}

/// Appends a new shutter to the add-on's own `shutters` option via the
/// Supervisor API and persists it to `/data/options.json`. Does not restart
/// the daemon -- call `restart_self` after, once the HTTP response for the
/// request that triggered this has had a chance to flush, since the restart
/// kills this process (and the ingress connection serving that response).
pub async fn add_shutter(id: &str, name: &str, mac: &str) -> Result<()> {
    let mut options = current_options().await?;
    let shutters = options
        .get_mut("shutters")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| anyhow!("no shutters array in current add-on options"))?;

    if shutters
        .iter()
        .any(|s| s.get("id").and_then(Value::as_str) == Some(id))
    {
        return Err(anyhow!("a shutter with id '{id}' already exists"));
    }

    shutters.push(json!({ "id": id, "name": name, "mac": mac }));

    let client = reqwest::Client::new();
    client
        .post(format!("{SUPERVISOR_API}/addons/self/options"))
        .bearer_auth(token()?)
        .json(&json!({ "options": options }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

pub async fn restart_self() -> Result<()> {
    let client = reqwest::Client::new();
    client
        .post(format!("{SUPERVISOR_API}/addons/self/restart"))
        .bearer_auth(token()?)
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}
