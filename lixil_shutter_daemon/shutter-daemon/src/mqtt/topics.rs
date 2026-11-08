pub fn command_topic(id: &str) -> String {
    format!("lixil_shutter/{id}/cover/set")
}

pub fn state_topic(id: &str) -> String {
    format!("lixil_shutter/{id}/cover/state")
}

pub fn availability_topic(id: &str) -> String {
    format!("lixil_shutter/{id}/availability")
}

pub fn needs_pairing_topic(id: &str) -> String {
    format!("lixil_shutter/{id}/needs_pairing")
}

pub const DAEMON_STATUS_TOPIC: &str = "lixil_shutter/daemon/status";

pub fn discovery_cover_topic(id: &str) -> String {
    format!("homeassistant/cover/lixil_shutter_{id}/config")
}

pub fn discovery_needs_pairing_topic(id: &str) -> String {
    format!("homeassistant/binary_sensor/lixil_shutter_{id}_needs_pairing/config")
}

/// Wildcard subscriptions used only to discover retained messages left
/// behind by shutter ids that are no longer configured (renamed/removed).
pub const STALE_SCAN_TOPICS: [&str; 3] = [
    "lixil_shutter/+/#",
    "homeassistant/cover/+/config",
    "homeassistant/binary_sensor/+/config",
];

/// Shutter id a topic belongs to, for any topic this daemon ever publishes
/// or subscribes to (state, availability, command, discovery). None for the
/// daemon-wide status topic and foreign topics.
pub fn owner_id(topic: &str) -> Option<&str> {
    let parts: Vec<&str> = topic.split('/').collect();
    match parts.as_slice() {
        ["lixil_shutter", "daemon", ..] => None,
        ["lixil_shutter", id, _, ..] => Some(id),
        ["homeassistant", "cover", obj, "config"] => obj.strip_prefix("lixil_shutter_"),
        ["homeassistant", "binary_sensor", obj, "config"] => obj
            .strip_prefix("lixil_shutter_")?
            .strip_suffix("_needs_pairing"),
        _ => None,
    }
}

/// Command topics are "lixil_shutter/{id}/cover/set"; anything else under
/// lixil_shutter/ (our own state echoes) must not be treated as a command.
pub fn command_id(topic: &str) -> Option<&str> {
    let parts: Vec<&str> = topic.split('/').collect();
    match parts.as_slice() {
        ["lixil_shutter", id, "cover", "set"] => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_ids() {
        assert_eq!(owner_id("lixil_shutter/living/cover/state"), Some("living"));
        assert_eq!(owner_id("lixil_shutter/living/availability"), Some("living"));
        assert_eq!(owner_id("lixil_shutter/daemon/status"), None);
        assert_eq!(owner_id(&discovery_cover_topic("bed_room")), Some("bed_room"));
        assert_eq!(owner_id(&discovery_needs_pairing_topic("bed_room")), Some("bed_room"));
        assert_eq!(owner_id("homeassistant/cover/other_thing/config"), None);
        assert_eq!(command_id(&command_topic("x")), Some("x"));
        assert_eq!(command_id(&state_topic("x")), None);
    }
}
