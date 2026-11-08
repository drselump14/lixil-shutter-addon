use serde_json::json;

use super::topics::{availability_topic, command_topic, needs_pairing_topic, state_topic, DAEMON_STATUS_TOPIC};

/// HA MQTT Cover discovery payload. `optimistic: true` combined with a
/// `state_topic` is explicitly supported by HA. The command handler publishes
/// "opening"/"closing" immediately (never a final "open"/"closed" guess --
/// confirmed live that publishing the final state on command receipt, before
/// the shutter actually finishes moving, shows the wrong state while it's
/// still visibly in motion). The final "open"/"closed" comes passively from
/// connection.rs's persistent status listener, whenever the device itself
/// pushes a fresh notification -- a request/reply Status probe's answer
/// doesn't reflect real physical position (confirmed live: a reply 2s after
/// sending Close already said "closed" mid-travel), so nothing here actively
/// polls for it.
pub fn cover_discovery_payload(id: &str, name: &str) -> serde_json::Value {
    json!({
        "name": null,
        "unique_id": format!("lixil_shutter_{id}"),
        "device_class": "shutter",
        "command_topic": command_topic(id),
        "state_topic": state_topic(id),
        "payload_open": "OPEN",
        "payload_close": "CLOSE",
        "payload_stop": "STOP",
        "state_open": "open",
        "state_closed": "closed",
        "state_opening": "opening",
        "state_closing": "closing",
        "optimistic": true,
        "availability_mode": "all",
        "availability": [
            {
                "topic": DAEMON_STATUS_TOPIC,
                "payload_available": "online",
                "payload_not_available": "offline"
            },
            {
                "topic": availability_topic(id),
                "payload_available": "online",
                "payload_not_available": "offline"
            }
        ],
        "device": device_block(id, name)
    })
}

pub fn needs_pairing_discovery_payload(id: &str, name: &str) -> serde_json::Value {
    json!({
        "name": "Needs pairing",
        "unique_id": format!("lixil_shutter_{id}_needs_pairing"),
        "device_class": "problem",
        "state_topic": needs_pairing_topic(id),
        "payload_on": "ON",
        "payload_off": "OFF",
        "availability": [
            {
                "topic": DAEMON_STATUS_TOPIC,
                "payload_available": "online",
                "payload_not_available": "offline"
            }
        ],
        "device": device_block(id, name)
    })
}

fn device_block(id: &str, name: &str) -> serde_json::Value {
    json!({
        "identifiers": [format!("lixil_shutter_{id}")],
        "name": name,
        "manufacturer": "Lixil",
        "model": "MyWindow Shutter"
    })
}
