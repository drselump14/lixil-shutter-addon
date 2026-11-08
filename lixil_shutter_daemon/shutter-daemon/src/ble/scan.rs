use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, Result};
use bluez_async::{AdapterId, CharacteristicId, DeviceId, DiscoveryFilter, MacAddress};
use tokio::time;
use tracing::{info, warn};
use uuid::Uuid;

use super::constants::{CONNECT_TIMEOUT, NOTIFY_UUID, SCAN_TIMEOUT, SERVICE_UUID, WRITE_UUID};
use super::session;

// Substring to match a specific adapter (e.g. a dedicated USB BT dongle),
// set once at startup from the add-on's `bluetooth_adapter` option -- see
// config.rs for why this exists (HA Core's own `bluetooth` integration can
// already own the onboard adapter).
static ADAPTER_HINT: OnceLock<String> = OnceLock::new();

pub fn set_adapter_hint(hint: String) {
    let _ = ADAPTER_HINT.set(hint);
}

/// Picks which adapter to use: if `bluetooth_adapter` names a substring,
/// match it against each adapter's MAC address first, then its `hciN` id as
/// a fallback; otherwise fall back to whichever adapter was enumerated last.
///
/// Matching by id alone (the original behaviour) is unsafe: `hciN` names are
/// assigned by USB enumeration order, which is not guaranteed stable across
/// reboots -- confirmed live, the physical dongle that was `hci1` before a
/// reboot came back as `hci0` after it, with the *other* (onboard) adapter
/// taking `hci1`. With a hint of "hci1" the daemon silently started talking
/// to HA Core's own onboard adapter instead of the dedicated dongle -- two
/// independent BlueZ clients contending for one adapter, which looks
/// exactly like (and was first mistaken for) the le-connection-abort-by-local
/// pattern this project already root-caused once for a *different* cause
/// (a second bluez_async session, see session.rs). The MAC address is
/// stable across reboots; the id is kept only so an existing "hci1"-style
/// hint value doesn't suddenly stop matching anything.
async fn select_adapter() -> Result<AdapterId> {
    let session = session::get()?;
    let adapters = session.get_adapters().await.map_err(|e| anyhow!(e))?;
    info!(
        adapters = ?adapters
            .iter()
            .map(|a| format!("{} ({})", a.id, a.mac_address))
            .collect::<Vec<_>>(),
        "available bluetooth adapters"
    );

    let hint = ADAPTER_HINT.get().map(String::as_str).unwrap_or("");
    if !hint.is_empty() {
        let hint_lower = hint.to_ascii_lowercase();
        for a in &adapters {
            if a.mac_address.to_string().to_ascii_lowercase().contains(&hint_lower) {
                info!(adapter = %a.id, mac = %a.mac_address, "selected adapter by MAC hint");
                return Ok(a.id.clone());
            }
        }
        for a in &adapters {
            if a.id.to_string().to_ascii_lowercase().contains(&hint_lower) {
                info!(adapter = %a.id, "selected adapter by id hint (not MAC -- unstable across reboots)");
                return Ok(a.id.clone());
            }
        }
        warn!(hint, "bluetooth_adapter hint matched no adapter -- falling back to last enumerated");
    }

    adapters
        .into_iter()
        .last()
        .map(|a| a.id)
        .ok_or_else(|| anyhow!("no bluetooth adapter found"))
}

/// Fresh scan for a specific device by address. Callers that already hold a
/// known `DeviceId` should try reconnecting that directly first and only
/// fall back to this on failure (see connection.rs) -- the id itself never
/// goes stale, so a plain `connect()` retry is cheap.
pub async fn find_advertising(address: &str) -> Result<Option<DeviceId>> {
    let session = session::get()?;
    let mac = MacAddress::from_str(address).map_err(|e| anyhow!(e))?;
    let adapter = select_adapter().await?;

    // BlueZ keeps every previously-seen device cached in its object tree
    // per-adapter indefinitely (not just for the life of one discovery
    // session) -- `bluetoothctl connect <mac>` relies on exactly this and
    // never triggers a fresh scan if the device is already known, which is
    // why it connected reliably (3/3) in live testing while this function's
    // old unconditional scan-then-connect sequence failed every time
    // (0/5, v0.2.4 and v0.2.5, even with a 2s post-scan settle -- so it
    // wasn't a stop-discovery timing race either). Running a *filtered*
    // discovery cycle at all, not just stopping it too soon, appears to
    // leave this adapter/driver in a state the controller can't connect
    // out of. Checking the cache first avoids running discovery at all on
    // the common case (device already seen since boot).
    if let Some(id) = find_in_known_devices(session, &adapter, &mac).await? {
        info!(address, "found in already-known devices, skipping scan");
        return Ok(Some(id));
    }

    let service_uuid = Uuid::from_str(SERVICE_UUID)?;
    info!(address, timeout_s = SCAN_TIMEOUT.as_secs(), "scanning for shutter");
    session
        .start_discovery_on_adapter_with_filter(
            &adapter,
            &DiscoveryFilter {
                service_uuids: vec![service_uuid],
                ..Default::default()
            },
        )
        .await
        .map_err(|e| anyhow!(e))?;
    time::sleep(SCAN_TIMEOUT).await;
    session
        .stop_discovery_on_adapter(&adapter)
        .await
        .map_err(|e| anyhow!(e))?;
    // Settle margin for the same stop-discovery race described above --
    // kept as defense in depth even though it alone didn't fix the issue.
    time::sleep(Duration::from_millis(2000)).await;

    find_in_known_devices(session, &adapter, &mac).await
}

async fn find_in_known_devices(
    session: &bluez_async::BluetoothSession,
    adapter: &AdapterId,
    mac: &MacAddress,
) -> Result<Option<DeviceId>> {
    let devices = session
        .get_devices_on_adapter(adapter)
        .await
        .map_err(|e| anyhow!(e))?;
    Ok(devices.into_iter().find(|d| d.mac_address == *mac).map(|d| d.id))
}

/// One shutter-shaped device seen while scanning for new (unconfigured)
/// shutters -- fed to the pairing UI so the user can pick the one they just
/// put into pairing mode.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoveredDevice {
    pub mac: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
}

/// Runs a filtered discovery for `duration` and returns every device seen
/// advertising the shutter service UUID that isn't already one of
/// `known_macs` -- i.e. candidates for "add new shutter". Callers must hold
/// the daemon's shared radio lock while this runs, same as connect/pair,
/// since discovery and an in-progress connect attempt contend for the same
/// adapter (see the module-level comments on `find_advertising` and on
/// `ShutterConnection::radio_lock`).
pub async fn discover_new(known_macs: &[String], duration: Duration) -> Result<Vec<DiscoveredDevice>> {
    let session = session::get()?;
    let adapter = select_adapter().await?;
    let service_uuid = Uuid::from_str(SERVICE_UUID)?;

    info!(duration_s = duration.as_secs(), "scanning for new shutters");
    session
        .start_discovery_on_adapter_with_filter(
            &adapter,
            &DiscoveryFilter {
                service_uuids: vec![service_uuid],
                ..Default::default()
            },
        )
        .await
        .map_err(|e| anyhow!(e))?;
    time::sleep(duration).await;
    session
        .stop_discovery_on_adapter(&adapter)
        .await
        .map_err(|e| anyhow!(e))?;

    let known: std::collections::HashSet<String> =
        known_macs.iter().map(|m| m.to_ascii_uppercase()).collect();
    let devices = session.get_devices_on_adapter(&adapter).await.map_err(|e| anyhow!(e))?;
    Ok(devices
        .into_iter()
        // The discovery filter only narrows what BlueZ reports live --
        // `get_devices_on_adapter` still returns every device it has ever
        // cached on this adapter, so re-check the service UUID ourselves.
        .filter(|d| d.services.contains(&service_uuid) || d.service_data.contains_key(&service_uuid))
        .filter(|d| !known.contains(&d.mac_address.to_string().to_ascii_uppercase()))
        .map(|d| DiscoveredDevice {
            mac: d.mac_address.to_string(),
            name: d.name.clone().or_else(|| d.alias.clone()),
            rssi: d.rssi,
        })
        .collect())
}

pub async fn connect(id: &DeviceId, address: &str) -> Result<bool> {
    let session = session::get()?;
    info!(address, timeout_s = CONNECT_TIMEOUT.as_secs(), "connecting");
    if let Err(e) = session.connect_with_timeout(id, CONNECT_TIMEOUT).await {
        warn!(address, error = %e, "connect failed");
        return Ok(false);
    }
    Ok(session
        .get_device_info(id)
        .await
        .map(|d| d.connected)
        .unwrap_or(false))
}

pub async fn find_characteristics(id: &DeviceId) -> Result<(CharacteristicId, CharacteristicId)> {
    let session = session::get()?;
    let service_uuid = Uuid::from_str(SERVICE_UUID)?;
    let write_uuid = Uuid::from_str(WRITE_UUID)?;
    let notify_uuid = Uuid::from_str(NOTIFY_UUID)?;

    let write_char = session
        .get_service_characteristic_by_uuid(id, service_uuid, write_uuid)
        .await
        .map_err(|e| anyhow!("write characteristic not found: {e}"))?;
    let notify_char = session
        .get_service_characteristic_by_uuid(id, service_uuid, notify_uuid)
        .await
        .map_err(|e| anyhow!("notify characteristic not found: {e}"))?;
    Ok((write_char.id, notify_char.id))
}

pub async fn disconnect(id: &DeviceId) {
    let Ok(session) = session::get() else {
        return;
    };
    let connected = session
        .get_device_info(id)
        .await
        .map(|d| d.connected)
        .unwrap_or(false);
    if connected {
        let _ = time::timeout(Duration::from_secs(3), session.disconnect(id)).await;
    }
}
