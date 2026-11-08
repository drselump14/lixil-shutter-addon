use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::timeout;
use tracing::{info, warn};

/// Registers a `NoInputNoOutput` BlueZ pairing agent for the daemon's
/// lifetime, by shelling out to `bluetoothctl` and keeping it running.
///
/// `bluez_async` (see session.rs) never registers an agent itself and has
/// no public API to -- `pair_with_timeout` calls `Device1.Pair()` directly,
/// so when BlueZ needs even a trivial Just-Works confirmation there's no
/// agent on the bus to ask, and it fails instantly with "Authentication
/// Failed". Confirmed live against Tatami Room: `Pair()` failed in under
/// 200ms, twice in a row, with the shutter's own pairing-mode button
/// freshly pressed both times -- too fast to be a real negotiation -- while
/// `bluetoothctl pair` against the same adapter+device succeeds, the only
/// difference being bluetoothctl registers its own agent first.
///
/// A spawned `bluetoothctl` only answers pairing prompts; it never scans,
/// connects, or touches GATT, so it doesn't recreate the two-D-Bus-clients-
/// contending-for-the-adapter regression documented in session.rs (that one
/// was about a second client actively driving its own connects).
pub async fn register() -> Result<Child> {
    let mut child = Command::new("bluetoothctl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("failed to spawn bluetoothctl: {e}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("no stdin on bluetoothctl child"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no stdout on bluetoothctl child"))?;
    let mut lines = BufReader::new(stdout).lines();

    // bluetoothctl needs a moment to connect to bluetoothd over D-Bus
    // before it acts on piped commands -- sending them immediately after
    // spawn risks it dropping them on the floor with zero error.
    tokio::time::sleep(Duration::from_millis(500)).await;
    stdin
        .write_all(b"agent NoInputNoOutput\ndefault-agent\n")
        .await?;
    // Leaked intentionally: bluetoothctl reads commands from this pipe as
    // they arrive and keeps running (and thus keeps the agent registered)
    // as long as the pipe stays open. Closing stdin would make it exit.
    std::mem::forget(stdin);

    // Confirm registration actually happened instead of trusting it
    // silently -- the first version of this did that, and pairing kept
    // failing instantly with no visibility into whether the agent was
    // ever really there.
    let (mut saw_agent, mut saw_default) = (false, false);
    let wait = timeout(Duration::from_secs(3), async {
        while let Ok(Some(line)) = lines.next_line().await {
            info!(bluetoothctl = %line, "agent setup output");
            saw_agent |= line.contains("Agent registered");
            saw_default |= line.contains("Default agent request successful");
            if saw_agent && saw_default {
                break;
            }
        }
    })
    .await;
    if wait.is_err() || !saw_agent || !saw_default {
        warn!(
            saw_agent,
            saw_default, "bluetoothctl agent registration did not confirm -- pairing will likely fail"
        );
    } else {
        info!("confirmed NoInputNoOutput pairing agent registered via bluetoothctl");
    }

    // Keep draining stdout for the daemon's lifetime so the pipe never
    // fills up and stalls bluetoothctl.
    tokio::spawn(async move {
        while let Ok(Some(line)) = lines.next_line().await {
            info!(bluetoothctl = %line, "agent output");
        }
    });

    Ok(child)
}
