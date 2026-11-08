mod ble;
mod bridge;
mod config;
mod mqtt;
mod shutter;
mod supervisor;
mod web;

use std::sync::Arc;

use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::EnvFilter;

use ble::Command;
use config::{AddonConfig, MqttConfig};
use shutter::ShutterManager;

const OPTIONS_PATH: &str = "/data/options.json";

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    debug: Option<DebugAction>,
}

/// Manual testing escape hatch, runnable via `docker exec`/`ha addons exec`,
/// filling the role shutter-ctl-rs's CLI plays today -- lets you test a
/// shutter's BLE path without going through MQTT at all.
#[derive(Subcommand)]
enum DebugAction {
    Send {
        id: String,
        #[arg(value_enum)]
        command: DebugCommand,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum DebugCommand {
    Open,
    Close,
    Stop,
    Status,
}

impl From<DebugCommand> for Command {
    fn from(c: DebugCommand) -> Self {
        match c {
            DebugCommand::Open => Command::Open,
            DebugCommand::Close => Command::Close,
            DebugCommand::Stop => Command::Stop,
            DebugCommand::Status => Command::Status,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addon_cfg = AddonConfig::load(OPTIONS_PATH)?;

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&addon_cfg.log_level))
        .init();

    ble::set_adapter_hint(addon_cfg.bluetooth_adapter.clone());
    // The daemon's single BlueZ D-Bus session -- every shutter's scan,
    // connect, pair and GATT operation goes through this one client. btleplug
    // has been dropped entirely: an earlier version kept it for scan/connect/
    // GATT and opened a second, independent bluez_async session just for
    // pairing, which dropped connect reliability from ~50% to 0/8 consecutive
    // le-connection-abort-by-local failures (see ble/session.rs).
    ble::session::init().await?;
    let _pairing_agent = ble::agent::register().await?;

    let manager = Arc::new(ShutterManager::new(addon_cfg.shutters));

    let args = Args::parse();
    if let Some(DebugAction::Send { id, command }) = args.debug {
        info!(id = %id, "debug send");
        manager.send_command(&id, command.into()).await?;
        return Ok(());
    }

    manager.spawn_background_tasks();
    tokio::spawn(web::serve(manager.clone()));

    let mqtt_cfg = MqttConfig::from_env();
    let (client, eventloop) = mqtt::client::build_client(&mqtt_cfg);

    tokio::spawn(bridge::run_state_mirror(client.clone(), manager.clone()));
    tokio::spawn(bridge::run_status_mirror(client.clone(), manager.clone()));

    let shutdown_client = client.clone();
    tokio::spawn(async move {
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");
        sigterm.recv().await;
        info!("SIGTERM received, publishing offline and shutting down");
        let _ = shutdown_client
            .publish(
                mqtt::topics::DAEMON_STATUS_TOPIC,
                rumqttc::QoS::AtLeastOnce,
                true,
                "offline",
            )
            .await;
        std::process::exit(0);
    });

    bridge::run_mqtt_loop(client, eventloop, manager).await;
    Ok(())
}
