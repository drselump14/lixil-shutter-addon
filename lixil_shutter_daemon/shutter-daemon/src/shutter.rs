use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{watch, Mutex};
use tracing::{info, warn};

use crate::ble::{Command, ConnState, ShutterConnection, ShutterError};
use crate::config::ShutterConfig;

pub struct ManagedShutter {
    pub name: String,
    pub conn: Arc<ShutterConnection>,
    pub state_rx: watch::Receiver<ConnState>,
    pub real_status_rx: watch::Receiver<Option<bool>>,
}

pub struct ShutterManager {
    shutters: HashMap<String, ManagedShutter>,
    // Shared with every ManagedShutter's ShutterConnection -- exposed so the
    // web scan handler can hold it too, serializing scan-for-new-devices
    // against any in-progress connect/pair attempt on the same adapter.
    radio_lock: Arc<Mutex<()>>,
}

/// One already-configured shutter, as shown in the "add new shutter" UI so
/// it can exclude devices already paired from the scan results.
pub struct ConfiguredShutter {
    pub id: String,
    pub name: String,
    pub mac: String,
}

const HEALTH_PROBE_INTERVAL: Duration = Duration::from_secs(600);
const BACKOFF_STEPS_SECS: [u64; 4] = [5, 15, 60, 300];

impl ShutterManager {
    pub fn new(configs: Vec<ShutterConfig>) -> Self {
        let radio_lock = Arc::new(Mutex::new(()));
        let mut shutters = HashMap::new();
        for cfg in configs {
            let (conn, rx, real_status_rx) =
                ShutterConnection::new(cfg.id.clone(), cfg.mac.clone(), radio_lock.clone());
            let conn = Arc::new(conn);
            shutters.insert(
                cfg.id.clone(),
                ManagedShutter {
                    name: cfg.name,
                    conn,
                    state_rx: rx,
                    real_status_rx,
                },
            );
        }
        Self { shutters, radio_lock }
    }

    pub fn ids(&self) -> Vec<String> {
        self.shutters.keys().cloned().collect()
    }

    pub fn get(&self, id: &str) -> Option<&ManagedShutter> {
        self.shutters.get(id)
    }

    pub fn radio_lock(&self) -> Arc<Mutex<()>> {
        self.radio_lock.clone()
    }

    pub fn configured(&self) -> Vec<ConfiguredShutter> {
        self.shutters
            .iter()
            .map(|(id, s)| ConfiguredShutter {
                id: id.clone(),
                name: s.name.clone(),
                mac: s.conn.mac.clone(),
            })
            .collect()
    }

    pub async fn send_command(&self, id: &str, cmd: Command) -> Result<(), ShutterError> {
        let shutter = self.shutters.get(id).ok_or(ShutterError::NotFound)?;
        shutter.conn.send_command(cmd).await
    }

    /// Spawns one background task per shutter: proactive reconnect-with-backoff
    /// when disconnected/needing pairing, plus a periodic health probe while
    /// bonded, so state (and therefore MQTT availability/needs_pairing) stays
    /// accurate even if nobody sends a command for a while.
    pub fn spawn_background_tasks(&self) {
        for (id, shutter) in &self.shutters {
            let id = id.clone();
            let conn = shutter.conn.clone();
            tokio::spawn(async move {
                let mut backoff_idx = 0usize;
                loop {
                    let state = conn.state();
                    match state {
                        ConnState::Bonded => {
                            backoff_idx = 0;
                            tokio::time::sleep(HEALTH_PROBE_INTERVAL).await;
                            if conn.probe_status().await.is_none() {
                                warn!(id = %id, "health probe got no reply -- bond may be dead");
                            }
                        }
                        _ => {
                            let delay = BACKOFF_STEPS_SECS
                                [backoff_idx.min(BACKOFF_STEPS_SECS.len() - 1)];
                            info!(id = %id, delay_s = delay, ?state, "retrying connection");
                            tokio::time::sleep(Duration::from_secs(delay)).await;
                            backoff_idx += 1;
                            // A failed attempt here just means try again later --
                            // send_command/probe_status already log the reason.
                            let _ = conn.probe_status().await;
                        }
                    }
                }
            });
        }
    }
}
