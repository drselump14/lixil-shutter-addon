use std::time::Duration;

pub const SERVICE_UUID: &str = "2141e110-213a-11e6-b67b-9e71128cae77";
pub const WRITE_UUID: &str = "2141e112-213a-11e6-b67b-9e71128cae77";
pub const NOTIFY_UUID: &str = "2141e111-213a-11e6-b67b-9e71128cae77";

pub const SCAN_TIMEOUT: Duration = Duration::from_secs(5);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const LISTEN_DURATION: Duration = Duration::from_secs(3);
// Bonding (notify subscribe) on an un-paired device just hangs instead of
// erroring -- confirmed live (a shutter not in pairing mode blocked the
// shared radio lock indefinitely, starving every other shutter's retries).
pub const BOND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Request current open/close state (needs an armed/bonded notify)
    Status,
    /// Request the list of paired remotes (needs an armed/bonded notify)
    PairingList,
    Open,
    Close,
    Stop,
    Fav1,
    Fav2,
}

impl Command {
    pub fn code(self) -> (u8, u8, u8) {
        match self {
            Command::Status => (0x03, 0x0B, 0x00),
            Command::PairingList => (0x03, 0x09, 0x00),
            Command::Open => (0x01, 0x03, 0x00),
            Command::Close => (0x01, 0x04, 0x00),
            Command::Stop => (0x01, 0x05, 0x00),
            Command::Fav1 => (0x01, 0x06, 0x01),
            Command::Fav2 => (0x01, 0x06, 0x02),
        }
    }

    pub fn from_mqtt_payload(payload: &str) -> Option<Self> {
        match payload {
            "OPEN" => Some(Command::Open),
            "CLOSE" => Some(Command::Close),
            "STOP" => Some(Command::Stop),
            "FAV1" => Some(Command::Fav1),
            "FAV2" => Some(Command::Fav2),
            _ => None,
        }
    }
}

pub fn make_payload(code: (u8, u8, u8), tag: u8) -> [u8; 4] {
    [code.0, code.1, code.2, tag]
}
