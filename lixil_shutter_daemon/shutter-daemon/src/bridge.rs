use std::sync::Arc;

use rumqttc::{AsyncClient, Event, Incoming, QoS};
use tracing::{error, info, warn};

use crate::ble::{Command, ConnState};
use crate::mqtt::{client as mqtt_client, topics};
use crate::shutter::ShutterManager;

/// Pumps the MQTT event loop: re-announces discovery on every (re)connect,
/// and dispatches incoming command-topic messages into the ShutterManager.
pub async fn run_mqtt_loop(
    client: AsyncClient,
    mut eventloop: rumqttc::EventLoop,
    manager: Arc<ShutterManager>,
) {
    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                info!("connected to MQTT broker");
                if let Err(e) = mqtt_client::announce_and_subscribe(&client, &manager).await {
                    error!(error = %e, "failed to announce discovery / subscribe");
                }
            }
            Ok(Event::Incoming(Incoming::Publish(p))) => {
                let topic = p.topic.clone();
                if p.retain {
                    purge_if_stale(&client, &manager, &topic, p.payload.is_empty()).await;
                }
                let Some(id) = topics::command_id(&topic).map(str::to_string) else {
                    continue;
                };
                if manager.get(&id).is_none() {
                    continue;
                }
                let payload = String::from_utf8_lossy(&p.payload).trim().to_uppercase();
                let Some(cmd) = Command::from_mqtt_payload(&payload) else {
                    warn!(topic, payload = %payload, "unrecognized command payload");
                    continue;
                };

                // Immediate feedback is "opening"/"closing", never a final
                // "open"/"closed" guess -- confirmed live that publishing the
                // final state on command receipt shows the wrong state while
                // the shutter is still visibly moving. The real correction
                // comes passively from ShutterConnection's persistent status
                // listener (connection.rs) whenever the device itself pushes
                // a fresh notification -- not from anything polled here.
                if let Some(optimistic) = optimistic_state(cmd) {
                    let _ = client
                        .publish(topics::state_topic(&id), QoS::AtLeastOnce, true, optimistic)
                        .await;
                }

                let manager = manager.clone();
                let client = client.clone();
                let id2 = id.clone();
                tokio::spawn(async move {
                    match manager.send_command(&id2, cmd).await {
                        Ok(()) => {
                            let _ = client
                                .publish(topics::needs_pairing_topic(&id2), QoS::AtLeastOnce, true, "OFF")
                                .await;
                            // The device never pushes an unsolicited "finished
                            // moving" notification -- confirmed against the
                            // official app's own ActionHandler, which re-polls
                            // getStatus() every 2s for as long as it's connected
                            // rather than waiting on one. Do the same here, now
                            // that the status bit positions are verified correct.
                            if let Some(shutter) = manager.get(&id2) {
                                let target_closed = match cmd {
                                    Command::Open => Some(false),
                                    Command::Close => Some(true),
                                    _ => None,
                                };
                                let max_wait = if target_closed.is_some() {
                                    std::time::Duration::from_secs(30)
                                } else {
                                    std::time::Duration::from_secs(8)
                                };
                                shutter.conn.poll_until_settled(target_closed, max_wait).await;
                            }
                        }
                        Err(e) => {
                            error!(id = %id2, error = %e, "command failed");
                            if matches!(e, crate::ble::ShutterError::NeedsPairing) {
                                let _ = client
                                    .publish(topics::needs_pairing_topic(&id2), QoS::AtLeastOnce, true, "ON")
                                    .await;
                            }
                        }
                    }
                });
            }
            Ok(_) => {}
            Err(e) => {
                warn!(error = %e, "MQTT event loop error, will retry");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    }
}

/// Mirrors each shutter's BLE ConnState into its MQTT availability +
/// needs_pairing topics, so the Lovelace UI reflects reality even when no
/// one has sent a command recently (fed by the background reconnect/health
/// tasks in shutter.rs via each connection's watch channel).
pub async fn run_state_mirror(client: AsyncClient, manager: Arc<ShutterManager>) {
    for id in manager.ids() {
        let Some(shutter) = manager.get(&id) else { continue };
        let mut rx = shutter.state_rx.clone();
        let client = client.clone();
        let id = id.clone();
        tokio::spawn(async move {
            loop {
                let state = *rx.borrow();
                let (avail, pairing) = match state {
                    ConnState::Bonded => ("online", "OFF"),
                    ConnState::NeedsPairing => ("online", "ON"),
                    ConnState::Connected | ConnState::Disconnected => ("offline", "OFF"),
                };
                let _ = client
                    .publish(topics::availability_topic(&id), QoS::AtLeastOnce, true, avail)
                    .await;
                let _ = client
                    .publish(topics::needs_pairing_topic(&id), QoS::AtLeastOnce, true, pairing)
                    .await;

                if rx.changed().await.is_err() {
                    break;
                }
            }
        });
    }
}

/// Mirrors each shutter's real (notify-confirmed) open/closed reading onto
/// the MQTT state topic -- fed by connection.rs's persistent status listener
/// (any notification the device sends, any time) and by the periodic health
/// probe. This is what corrects the "opening"/"closing" optimistic guess
/// from run_mqtt_loop once something real comes in.
pub async fn run_status_mirror(client: AsyncClient, manager: Arc<ShutterManager>) {
    for id in manager.ids() {
        let Some(shutter) = manager.get(&id) else { continue };
        let mut rx = shutter.real_status_rx.clone();
        let client = client.clone();
        let id = id.clone();
        tokio::spawn(async move {
            loop {
                let current: Option<bool> = *rx.borrow();
                if let Some(closed) = current {
                    let state = if closed { "closed" } else { "open" };
                    let _ = client
                        .publish(topics::state_topic(&id), QoS::AtLeastOnce, true, state)
                        .await;
                }
                if rx.changed().await.is_err() {
                    break;
                }
            }
        });
    }
}

/// A retained message for a shutter id that's no longer configured (id was
/// renamed or the shutter removed) is wiped by publishing an empty retained
/// payload. For discovery topics that also makes HA delete the ghost entity
/// (and its device once empty); for state/availability it stops the old
/// entity looking "online" with a stale position.
async fn purge_if_stale(client: &AsyncClient, manager: &ShutterManager, topic: &str, empty: bool) {
    if empty {
        return;
    }
    let Some(id) = topics::owner_id(topic) else { return };
    if manager.get(id).is_some() {
        return;
    }
    info!(topic, stale_id = id, "clearing retained topic of unconfigured shutter id");
    let _ = client.publish(topic, QoS::AtLeastOnce, true, Vec::<u8>::new()).await;
}

fn optimistic_state(cmd: Command) -> Option<&'static str> {
    match cmd {
        Command::Open => Some("opening"),
        Command::Close => Some("closing"),
        _ => None,
    }
}
