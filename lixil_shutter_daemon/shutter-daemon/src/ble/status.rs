/// Typed replacement for the old CLI's print-only notification decoding
/// (shutter-ctl-rs's `print_notification`) -- byte 2: bit 3 = open/close,
/// bit 2 = "saihu" (transitional/unknown). Confirmed against the official
/// MyWindow Android app's decompiled `ShutterClient.fetchStatus()`: it reads
/// `toBinaryString(value[2])`, whose charAt(0) is bit 7 (MSB-first), and
/// checks charAt(5) (= bit 2) for saihu, charAt(4) (= bit 3) for open/close.
/// Our original bit 4/bit 5 guess was off by exactly one position -- it
/// happened to read a bit that was always 1 regardless of real state, which
/// is why status looked "stuck" no matter what command was sent (confirmed
/// live: identical bit4 reading after both Open and Close commands).
#[derive(Debug, Clone)]
pub struct ShutterStatus {
    pub raw: Vec<u8>,
    pub open_close_bit: Option<bool>,
    pub saihu_bit: Option<bool>,
}

impl ShutterStatus {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 3 {
            return None;
        }
        let b2 = data[2];
        Some(ShutterStatus {
            raw: data.to_vec(),
            open_close_bit: Some(((b2 >> 3) & 1) == 1),
            saihu_bit: Some(((b2 >> 2) & 1) == 1),
        })
    }

    /// True if closed, false if open, None if "saihu" (transitional/unknown).
    /// Polarity (bit=1 means closed) matches the official app's
    /// `status_close` branch firing when this bit is '1'.
    pub fn is_closed(&self) -> Option<bool> {
        if self.saihu_bit == Some(true) {
            return None;
        }
        self.open_close_bit
    }
}
