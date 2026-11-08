use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bluez_async::{BluetoothEvent, CharacteristicEvent, CharacteristicId, DeviceId, WriteOptions, WriteType};
use thiserror::Error;
use tokio::sync::{watch, Mutex};
use tracing::{info, warn};

use super::constants::{make_payload, BOND_TIMEOUT, Command, LISTEN_DURATION};
use super::scan::{connect, disconnect, find_advertising, find_characteristics};
use super::session;
use super::status::ShutterStatus;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnState {
    Disconnected,
    Connected,
    Bonded,
    NeedsPairing,
}

#[derive(Error, Debug)]
pub enum ShutterError {
    #[error("shutter not currently advertising/reachable")]
    NotFound,
    #[error("connect failed or timed out")]
    ConnectFailed,
    #[error("bonding failed -- press the physical pairing-mode button on this shutter, then retry")]
    NeedsPairing,
    #[error("BLE operation failed: {0}")]
    Other(#[from] anyhow::Error),
}

struct Inner {
    device_id: Option<DeviceId>,
    write_char: Option<CharacteristicId>,
    notify_char: Option<CharacteristicId>,
    tag: u8,
}

/// Holds one shutter's BLE connection open indefinitely, independent of any
/// single command -- mirrors the Python integration's LixilShutterDevice:
/// no idle timeout, one lock serializing connect/bond/write, a clock-seeded
/// tag counter (the device's replay protection silently drops a reused tag,
/// so a fixed tag like the old CLI's `tag=1` would look like a successful
/// write while doing nothing).
pub struct ShutterConnection {
    pub id: String,
    pub mac: String,
    inner: Mutex<Inner>,
    status_tx: watch::Sender<ConnState>,
    real_status_tx: watch::Sender<Option<bool>>,
    // A single physical BT adapter can't establish multiple LE connections at
    // once -- BlueZ aborts overlapping attempts (confirmed: concurrent
    // connects to all 3 shutters all failed with le-connection-abort-by-local).
    // Shared across every ShutterConnection so scan+connect+bond is serialized
    // adapter-wide; once bonded, writes/reads proceed without this lock.
    radio_lock: Arc<Mutex<()>>,
}

impl ShutterConnection {
    pub fn new(
        id: String,
        mac: String,
        radio_lock: Arc<Mutex<()>>,
    ) -> (Self, watch::Receiver<ConnState>, watch::Receiver<Option<bool>>) {
        let (tx, rx) = watch::channel(ConnState::Disconnected);
        let (real_tx, real_rx) = watch::channel(None);
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| (d.as_secs() % 100) as u8)
            .unwrap_or(0);
        (
            Self {
                id,
                mac,
                inner: Mutex::new(Inner {
                    device_id: None,
                    write_char: None,
                    notify_char: None,
                    tag: seed,
                }),
                status_tx: tx,
                real_status_tx: real_tx,
                radio_lock,
            },
            rx,
            real_rx,
        )
    }

    pub fn state(&self) -> ConnState {
        *self.status_tx.borrow()
    }

    fn set_state(&self, s: ConnState) {
        let _ = self.status_tx.send(s);
    }

    /// Ensures a live, bonded connection, reconnecting/re-scanning as needed.
    /// Everything here -- scan, connect, pair, GATT -- goes through the
    /// single process-wide `bluez_async` session in `session.rs`. An earlier
    /// version used btleplug for scan/connect/GATT (which never calls
    /// BlueZ's real `Device1.Pair()` -- confirmed by reading its source) and
    /// opened a second, independent bluez_async session just for pairing.
    /// That second session caused a severe connect-reliability regression
    /// (0/8 consecutive le-connection-abort-by-local failures, confirmed not
    /// a wedged controller via a clean hci1 power-cycle), consistent with
    /// two independent D-Bus clients contending for the same adapter.
    /// Dropping btleplug and doing everything through one session removes
    /// that contention instead of working around it. A pairing/bonding
    /// failure here means the physical pairing-mode button needs pressing --
    /// that failure must propagate as a real error, never be swallowed,
    /// since a write over an unbonded link reports success with no
    /// exception while the shutter never moves.
    async fn ensure_bonded(&self, locked: &mut Inner) -> Result<(), ShutterError> {
        if let Some(id) = &locked.device_id {
            if let Ok(session) = session::get() {
                let connected = session
                    .get_device_info(id)
                    .await
                    .map(|d| d.connected)
                    .unwrap_or(false);
                if connected && locked.notify_char.is_some() && self.state() == ConnState::Bonded {
                    // Already connected with characteristics resolved from a
                    // prior bonded session in this process's lifetime -- trust it.
                    return Ok(());
                }
            }
        }

        let _radio = self.radio_lock.lock().await;
        let device_id = match &locked.device_id {
            Some(id) if connect(id, &self.mac).await.unwrap_or(false) => id.clone(),
            _ => {
                let found = find_advertising(&self.mac)
                    .await
                    .map_err(ShutterError::Other)?;
                let id = found.ok_or(ShutterError::NotFound)?;
                if !connect(&id, &self.mac).await.map_err(ShutterError::Other)? {
                    self.set_state(ConnState::Disconnected);
                    return Err(ShutterError::ConnectFailed);
                }
                id
            }
        };
        self.set_state(ConnState::Connected);

        if let Err(e) = session::ensure_paired(&device_id, BOND_TIMEOUT).await {
            warn!(id = %self.id, error = %e, "pairing failed -- needs physical pairing-mode button press");
            disconnect(&device_id).await;
            locked.device_id = None;
            locked.write_char = None;
            locked.notify_char = None;
            self.set_state(ConnState::NeedsPairing);
            return Err(ShutterError::NeedsPairing);
        }

        let (write_char, notify_char) = find_characteristics(&device_id)
            .await
            .map_err(ShutterError::Other)?;

        info!(id = %self.id, "arming notifications");
        let bt = session::get().map_err(ShutterError::Other)?;
        let armed = tokio::time::timeout(BOND_TIMEOUT, bt.start_notify(&notify_char)).await;
        match armed {
            Ok(Ok(())) => {
                info!(id = %self.id, "bonded");
                self.spawn_status_listener(notify_char.clone());
                locked.device_id = Some(device_id);
                locked.write_char = Some(write_char);
                locked.notify_char = Some(notify_char);
                self.set_state(ConnState::Bonded);
                Ok(())
            }
            Ok(Err(e)) => {
                warn!(id = %self.id, error = %e, "arming notifications failed -- needs physical pairing-mode button press");
                disconnect(&device_id).await;
                locked.device_id = None;
                locked.write_char = None;
                locked.notify_char = None;
                self.set_state(ConnState::NeedsPairing);
                Err(ShutterError::NeedsPairing)
            }
            Err(_) => {
                warn!(id = %self.id, "arming notifications timed out -- needs physical pairing-mode button press");
                disconnect(&device_id).await;
                locked.device_id = None;
                locked.write_char = None;
                locked.notify_char = None;
                self.set_state(ConnState::NeedsPairing);
                Err(ShutterError::NeedsPairing)
            }
        }
    }

    /// Listens continuously on the notify characteristic for the life of
    /// this bonded connection, publishing every status reading the moment
    /// the device sends it -- not just replies to our own Status requests.
    /// Needed because a Status *request*'s reply doesn't reflect real
    /// physical position: confirmed live, a reply arrived 2s after sending
    /// Close already reporting "closed" while the shutter was still
    /// visibly moving. Whatever the device pushes on its own (e.g. a real
    /// limit-switch trigger once it actually finishes) is the only thing
    /// that can be trusted for "did it actually finish moving" -- polling
    /// our own request repeatedly just gets the same untrustworthy answer
    /// faster.
    fn spawn_status_listener(&self, notify_char: CharacteristicId) {
        let real_status_tx = self.real_status_tx.clone();
        let id = self.id.clone();
        tokio::spawn(async move {
            let Ok(session) = session::get() else { return };
            let Ok(mut events) = session.characteristic_event_stream(&notify_char).await else {
                return;
            };
            while let Some(event) = futures::StreamExt::next(&mut events).await {
                if let BluetoothEvent::Characteristic {
                    id: ev_id,
                    event: CharacteristicEvent::Value { value },
                } = event
                {
                    if ev_id != notify_char {
                        continue;
                    }
                    if let Some(status) = ShutterStatus::parse(&value) {
                        info!(id = %id, ?status, "status notification");
                        let _ = real_status_tx.send(status.is_closed());
                    }
                }
            }
            info!(id = %id, "status notification stream ended");
        });
    }

    fn next_tag(locked: &mut Inner) -> u8 {
        locked.tag = (locked.tag + 1) % 100;
        locked.tag
    }

    pub async fn send_command(&self, cmd: Command) -> Result<(), ShutterError> {
        let mut locked = self.inner.lock().await;
        self.ensure_bonded(&mut locked).await?;

        let tag = Self::next_tag(&mut locked);
        let payload = make_payload(cmd.code(), tag);
        let write_char = locked.write_char.clone().ok_or(ShutterError::ConnectFailed)?;
        drop(locked);

        let session = session::get().map_err(ShutterError::Other)?;
        session
            .write_characteristic_value_with_options(
                &write_char,
                payload.to_vec(),
                WriteOptions {
                    write_type: Some(WriteType::WithoutResponse),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| ShutterError::Other(anyhow::anyhow!(e)))?;
        info!(id = %self.id, payload = %hex::encode(payload), "command written");
        Ok(())
    }

    /// Actively re-probes Status every 2s until it matches `target_closed`
    /// or `max_duration` elapses. Mirrors the official MyWindow app's own
    /// `ActionHandler.runHandler()` (decompiled APK): it calls `getStatus()`
    /// in a loop every 2000ms for as long as it's connected -- the device
    /// never pushes an unsolicited "finished moving" notification on its
    /// own, so a correct reading only ever comes from actively asking again.
    /// Safe to do now that `ShutterStatus`'s bit positions are verified
    /// against that same app's `fetchStatus()` (byte 2, bit 3/bit 2) --
    /// an earlier version of this loop was removed because it was built on
    /// the wrong bits and settled on a false reading instantly.
    pub async fn poll_until_settled(&self, target_closed: Option<bool>, max_duration: std::time::Duration) {
        let deadline = tokio::time::Instant::now() + max_duration;
        loop {
            if let Some(status) = self.probe_status().await {
                if let (Some(target), Some(actual)) = (target_closed, status.is_closed()) {
                    if actual == target {
                        return;
                    }
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    /// Periodic health probe: confirms the bond is still real (not just a
    /// stale D-Bus-looking handle) without ever moving the shutter, and
    /// publishes the real open/closed reading so the MQTT state topic can
    /// correct any stale optimistic guess.
    pub async fn probe_status(&self) -> Option<ShutterStatus> {
        let mut locked = self.inner.lock().await;
        if self.ensure_bonded(&mut locked).await.is_err() {
            return None;
        }
        let tag = Self::next_tag(&mut locked);
        let payload = make_payload(Command::Status.code(), tag);
        let write_char = locked.write_char.clone()?;
        let notify_char = locked.notify_char.clone()?;
        drop(locked);

        let session = session::get().ok()?;
        let mut events = session.characteristic_event_stream(&notify_char).await.ok()?;
        if session
            .write_characteristic_value_with_options(
                &write_char,
                payload.to_vec(),
                WriteOptions {
                    write_type: Some(WriteType::WithoutResponse),
                    ..Default::default()
                },
            )
            .await
            .is_err()
        {
            return None;
        }

        let deadline = tokio::time::Instant::now() + LISTEN_DURATION;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            match tokio::time::timeout(remaining, futures::StreamExt::next(&mut events)).await {
                Ok(Some(BluetoothEvent::Characteristic {
                    id,
                    event: CharacteristicEvent::Value { value },
                })) if id == notify_char => {
                    let status = ShutterStatus::parse(&value);
                    if let Some(s) = &status {
                        let _ = self.real_status_tx.send(s.is_closed());
                    }
                    return status;
                }
                Ok(Some(_)) => continue,
                _ => return None,
            }
        }
    }
}
