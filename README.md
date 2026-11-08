# Lixil MyWindow Shutter Daemon (HAOS add-on)

Persistent BLE connection to Lixil MyWindow smart shutters, bridged to
Home Assistant over MQTT. Runs as its own HAOS add-on (its own Docker
container, managed by Supervisor) specifically so Home Assistant Core
restarts — which happen often for routine updates — don't tear down the
BLE connection.

## Install / upgrade

1. Settings → Add-ons → Add-on Store → ⋮ → Repositories → add
   `https://github.com/drselump14/lixil-shutter-addon`.
2. Install **Lixil MyWindow Shutter Daemon**.

Releasing: bump `version` in `lixil_shutter_daemon/config.yaml` and push
to `main`. The `Build add-on image` workflow builds the aarch64 image on a
native ARM runner and pushes `ghcr.io/drselump14/aarch64-lixil-shutter-daemon:<version>`.
HA then shows an Update button (Add-on Store → ⋮ → Check for updates to
see it immediately). The ghcr package must be **public** so Supervisor can
pull it without credentials.

## Why this exists

This shutter's firmware invalidates its BLE bond on every disconnect,
requiring a physical pairing-mode button press before the next
connection will actually move the shutter (a write over an unbonded
link reports success with no error, but the shutter doesn't move —
confirmed by testing). Previously the BLE connection lived inside Home
Assistant Core's own process (`lixil-shutter-ha`, a custom Python
integration) — every Core restart tore it down and forced a fresh
pair, meaning a button press every time Core updated (a few times a
day). Add-ons run in a separate container that Core restarts don't
touch, so the connection here survives Core restarts/updates — only an
update to *this* add-on, or a genuine BLE link drop, needs the button
again.

Builds on `../shutter-ctl-rs`, a one-shot Rust CLI that proved the
protocol on Linux/BlueZ. That CLI remains useful standalone (e.g. a
quick manual test from a laptop with its own BT dongle) — this add-on
vendors its connection/protocol logic and extends it into a persistent
per-shutter connection manager bridged to MQTT instead of a one-shot
scan-connect-act-disconnect cycle.

## Architecture

- `lixil_shutter_daemon/shutter-daemon/` — the Rust daemon. One `ShutterConnection` per
  configured shutter, held open indefinitely (no idle timeout, by
  design — matching the retired Python integration's approach), with
  background reconnect-with-backoff and a periodic health probe.
- MQTT topics under `lixil_shutter/{id}/...`; HA discovers the 3
  `cover` entities (plus a `needs_pairing` diagnostic `binary_sensor`
  per shutter) automatically via MQTT Discovery — no custom Home
  Assistant integration needed.
- Broker connection comes from Supervisor's `mqtt:want` service
  discovery, not from add-on options.

See `src/main.rs`'s `DebugAction` subcommand for manual testing without
MQTT (`docker exec` / `ha addons exec` into the container, then
`shutter-daemon debug send <id> open`).

### Adding a shutter from the add-on's UI

The add-on exposes an Ingress panel (`src/web.rs` + `assets/ingress.html`)
for adding a new shutter without hand-editing YAML: press the physical
pairing-mode button on the shutter, open the panel from the add-on's page
in Settings → Add-ons, hit "Scan for shutters", pick it from the results,
give it an id/name, and save. The daemon writes the new entry to its own
`shutters` option via the Supervisor API (`POST /addons/self/options`) and
restarts itself (`POST /addons/self/restart`) to pick it up — both calls
use the `SUPERVISOR_TOKEN` every add-on gets for free, scoped to its own
`/addons/self/*` endpoints, so no extra `hassio_api` permission is needed.

## Status

Scaffolded and type-checked (`cargo check`, macOS/CoreBluetooth
backend) but **not yet built or tested for the actual target
(aarch64 HAOS, BlueZ)**. Before trusting this in production, work
through the verification phases below in order — each is designed to
need at most one physical pairing-button press.

1. Scaffold only (no BLE) — confirm the add-on builds for aarch64 and
   starts under Supervisor, MQTT creds resolve correctly.
2. MQTT only — confirm the 3 cover + 3 diagnostic entities appear in
   HA via discovery, showing `unavailable`.
3. BLE connection manager, Living Room only, via the debug CLI (no
   MQTT) — one button press, confirm bonding succeeds.
4. Bridge BLE+MQTT, Living Room — confirm Open/Close/Stop from the HA
   UI actually move it; restart the add-on container and confirm no
   new button press is needed.
5. Repeat 3-4 for Master Bedroom and Tatami Room, one button press
   each — this is also where BLE range over the Pi's onboard adapter
   for the two farther rooms gets its first real test (advertisement
   visibility was confirmed earlier; real connection stability was
   not). If unstable, that's a known accepted risk to fix later
   (relocate the Pi, add a second USB BT dongle) — not a blocker.
6. Retire `../lixil-shutter-ha`'s `custom_components/lixil_shutter/`
   on the HAOS host once the above are confirmed.
7. **Acceptance test**: for each shutter, restart HA Core
   (`ha_restart`), then — without pressing any physical button — send
   a command and confirm it physically moves. This is the actual point
   of this project.

## Open items to verify at real build/deploy time

- `build.yaml`'s base image tag (`ghcr.io/hassio-addons/base-debian`)
  and the `host_dbus`/`services` fields in `config.yaml` should be
  checked against current Supervisor docs — these have shifted across
  versions. If `host_dbus: true` alone isn't enough for BLE scanning to
  work, try adding `privileged: ["NET_ADMIN"]` before reaching for
  `full_access: true`.
- `ShutterStatus`'s bit polarity (`is_closed()` assumes bit 4 of byte 2
  means "closed") is inferred from the retired Python integration's
  constant naming, not independently re-verified — confirm against a
  real shutter during phase 4/5 above.
