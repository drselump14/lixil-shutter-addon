use anyhow::Result;
use rumqttc::{AsyncClient, LastWill, MqttOptions, QoS};
use tracing::info;

use crate::config::MqttConfig;
use crate::shutter::ShutterManager;

use super::topics::{command_topic, DAEMON_STATUS_TOPIC};
use super::{discovery, topics};

pub fn build_client(cfg: &MqttConfig) -> (AsyncClient, rumqttc::EventLoop) {
    let mut opts = MqttOptions::new("lixil_shutter_daemon", &cfg.host, cfg.port);
    if let (Some(u), Some(p)) = (&cfg.username, &cfg.password) {
        opts.set_credentials(u, p);
    }
    opts.set_last_will(LastWill::new(DAEMON_STATUS_TOPIC, "offline", QoS::AtLeastOnce, true));
    opts.set_keep_alive(std::time::Duration::from_secs(30));
    AsyncClient::new(opts, 32)
}

/// Publishes MQTT Discovery for every configured shutter and subscribes to
/// each one's command topic. Called once on (re)connect.
pub async fn announce_and_subscribe(client: &AsyncClient, manager: &ShutterManager) -> Result<()> {
    client
        .publish(DAEMON_STATUS_TOPIC, QoS::AtLeastOnce, true, "online")
        .await?;

    for id in manager.ids() {
        let Some(shutter) = manager.get(&id) else { continue };

        let cover_payload = discovery::cover_discovery_payload(&id, &shutter.name);
        client
            .publish(
                topics::discovery_cover_topic(&id),
                QoS::AtLeastOnce,
                true,
                serde_json::to_vec(&cover_payload)?,
            )
            .await?;

        let pairing_payload = discovery::needs_pairing_discovery_payload(&id, &shutter.name);
        client
            .publish(
                topics::discovery_needs_pairing_topic(&id),
                QoS::AtLeastOnce,
                true,
                serde_json::to_vec(&pairing_payload)?,
            )
            .await?;

        client
            .subscribe(command_topic(&id), QoS::AtLeastOnce)
            .await?;

        info!(id = %id, "announced discovery + subscribed to command topic");
    }

    // Retained leftovers from renamed/removed shutter ids arrive on these
    // with the retain flag set; bridge.rs clears them (see purge_if_stale).
    for t in topics::STALE_SCAN_TOPICS {
        client.subscribe(t, QoS::AtLeastOnce).await?;
    }
    Ok(())
}
