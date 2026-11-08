use std::time::Duration;

use anyhow::{anyhow, Result};
use bluez_async::{BluetoothSession, DeviceId};
use tokio::sync::OnceCell;
use tracing::{error, info};

static SESSION: OnceCell<BluetoothSession> = OnceCell::const_new();

/// Opens the daemon's single BlueZ D-Bus session -- shared by every shutter
/// for scanning, connecting, pairing, and all GATT reads/writes/notifies.
/// Deliberately the ONLY BlueZ client this process runs. An earlier version
/// kept btleplug for scan/connect/GATT and opened a second, independent
/// bluez_async session just for pairing; that second session dropped connect
/// reliability from ~50% to 0/8 consecutive le-connection-abort-by-local
/// failures, even surviving a clean hci1 power-cycle that ruled out a wedged
/// controller -- strong evidence two independent D-Bus clients contend for
/// the same adapter in a way the existing radio_lock mutex (which only
/// serializes our own call sequence) can't protect against. Doing
/// everything through bluez_async removes the second session instead of
/// working around it.
pub async fn init() -> Result<()> {
    let (handle, session) = BluetoothSession::new().await.map_err(|e| anyhow!(e))?;
    tokio::spawn(async move {
        if let Err(e) = handle.await {
            error!(error = %e, "bluez D-Bus session lost -- daemon needs restart");
        }
    });
    SESSION
        .set(session)
        .map_err(|_| anyhow!("bluez session already initialized"))?;
    Ok(())
}

pub fn get() -> Result<&'static BluetoothSession> {
    SESSION
        .get()
        .ok_or_else(|| anyhow!("bluez session not initialized"))
}

/// Ensures the given device is bonded, issuing an explicit Pair() if it
/// isn't already. btleplug never calls BlueZ's real `Device1.Pair()`
/// (confirmed by reading its source) -- it only relies on a GATT
/// subscribe implicitly triggering bonding, which hung/failed 3/3 live on
/// this host's USB dongle even with the shutter's pairing-mode button
/// freshly pressed, while `bluetoothctl pair` against the same adapter
/// succeeded instantly. BlueZ's `paired` property makes this idempotent, so
/// callers can call it on every reconnect without re-pairing an
/// already-bonded device.
pub async fn ensure_paired(id: &DeviceId, timeout: Duration) -> Result<()> {
    let session = get()?;
    let info = session.get_device_info(id).await.map_err(|e| anyhow!(e))?;
    if info.paired {
        return Ok(());
    }
    info!(device = %id, "pairing (explicit BlueZ Pair())");
    session
        .pair_with_timeout(id, timeout)
        .await
        .map_err(|e| anyhow!(e))
}
