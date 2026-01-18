mod alsa_silence;
mod audio;
mod config;
mod discovery;
mod health;
mod install;
mod models;
mod server_api;
mod stream;
mod timestamp;

use anyhow::{Context, Result};
use std::time::Duration;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    alsa_silence::init();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_usage();
        eprintln!();
        anyhow::bail!("missing command (see usage above)");
    }

    match args[1].as_str() {
        "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        "--version" | "-V" => {
            println!("lox-linein-bridge {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "install" => install::run_install().await,
        "run" => run().await,
        _ => {
            print_usage();
            anyhow::bail!("unknown command");
        }
    }
}

async fn run() -> Result<()> {
    let (config, path) = config::load_or_create_config()?;
    info!("loaded config from {}", path.display());
    let hostname = hostname::get()
        .unwrap_or_else(|_| "unknown".into())
        .to_string_lossy()
        .to_string();
    let (ip, mac) = local_identity()?;

    let server = loop {
        match discovery::discover_server(
            config.preferred_server_name.as_deref(),
            config.preferred_server_mac.as_deref(),
        ) {
            Ok(server) => {
                info!("discovered server: {}", server.base_url);
                break server;
            }
            Err(err) => {
                warn!("mDNS discovery failed: {}", err);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };

    let api =
        server_api::ServerApi::new(&server.base_url, &server.register_path, &server.status_path)?;
    info!("server: {}", server.base_url);

    let capture_devices = audio::list_input_device_details()?;
    let register = models::BridgeRegisterRequest {
        bridge_id: config.bridge_id.clone(),
        hostname,
        version: env!("CARGO_PKG_VERSION").to_string(),
        ip: ip.clone(),
        mac: mac.clone(),
        capture_devices: capture_devices.clone(),
    };
    info!("registering bridge {}", config.bridge_id);
    let initial_config = api.register_bridge(&register).await?;
    info!(
        "registration response: assigned_input_id={:?}, capture_device={:?}",
        initial_config.assigned_input_id, initial_config.capture_device
    );

    let runtime = RuntimeConfig::from_response(initial_config);
    let (config_tx, mut config_rx) = tokio::sync::watch::channel(runtime.clone());

    let status = stream::StatusHandle::new("", "");
    health::spawn(status.clone());

    let status_api = api.clone();
    let bridge_id = config.bridge_id.clone();
    let status_handle = status.clone();
    tokio::spawn(async move {
        let mut runtime = runtime;
        let mut last_devices_hash = None;
        let mut devices = capture_devices;
        loop {
            let mut snapshot = status_handle.bridge_status();
            let current_hash = hash_capture_devices(&devices);
            if last_devices_hash != Some(current_hash) {
                snapshot.capture_devices = Some(devices.clone());
                last_devices_hash = Some(current_hash);
            }
            match status_api.post_status(&bridge_id, &snapshot).await {
                Ok(update) => {
                    if let Some(updated) = runtime.update(update) {
                        info!(
                            "config update: assigned_input_id={:?}, capture_device={:?}, vad_threshold_db={}, vad_hold_ms={}",
                            updated.assigned_input_id,
                            updated.capture_device,
                            updated.vad_threshold_db,
                            updated.vad_hold_ms
                        );
                        let _ = config_tx.send(updated);
                    }
                }
                Err(err) => {
                    tracing::debug!("status post failed: {}", err);
                }
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
            if let Ok(new_devices) = audio::list_input_device_details() {
                devices = new_devices;
            }
        }
    });

    let mut backoff = Backoff::new();
    loop {
        let current = config_rx.borrow().clone();
        if !current.is_ready() {
            status.set_state("IDLE");
            config_rx.changed().await?;
            continue;
        }
        let ingest = match current.ingest_target() {
            Some(target) => target,
            None => {
                status.set_state("IDLE");
                config_rx.changed().await?;
                continue;
            }
        };
        let capture_device = current.capture_device.clone().unwrap_or_default();
        status.set_device(&capture_device);
        status.set_ingest(&current.ingest_label());

        match audio::start_capture(&capture_device) {
            Ok(session) => {
                backoff.reset();
                status.set_capture_info(
                    session.sample_rate,
                    session.channels,
                    format!("{:?}", session.format),
                );
                let audio::CaptureSession {
                    receiver,
                    error_receiver,
                    stream,
                    ..
                } = session;
                let _stream_guard = stream;
                let params = stream::StreamParams {
                    ingest,
                    rx: receiver,
                    err_rx: error_receiver,
                    threshold_db: current.vad_threshold_db,
                    hold_duration: std::time::Duration::from_millis(current.vad_hold_ms),
                    status: status.clone(),
                };

                let mut stream_task =
                    tokio::spawn(async move { stream::stream_audio(params).await });
                tokio::select! {
                    result = &mut stream_task => {
                        match result.context("stream task join")? {
                            Ok(()) => {}
                            Err(err) => {
                                status.set_state("ERROR");
                                status.set_last_error(Some(err.to_string()));
                                warn!("streaming stopped: {}", err);
                            }
                        }
                    }
                    _ = config_rx.changed() => {
                        stream_task.abort();
                    }
                }
            }
            Err(err) => {
                status.set_state("ERROR");
                status.set_last_error(Some(err.to_string()));
                warn!("capture failed: {}", err);
                tokio::time::sleep(backoff.next_delay()).await;
            }
        }
    }
}

fn print_usage() {
    eprintln!("Usage:");
    eprintln!("  lox-linein-bridge install");
    eprintln!("  lox-linein-bridge run");
    eprintln!("  lox-linein-bridge --help");
    eprintln!("  lox-linein-bridge --version");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  lox-linein-bridge install");
    eprintln!("  lox-linein-bridge run");
}

#[derive(Debug, Clone)]
struct RuntimeConfig {
    assigned_input_id: Option<String>,
    ingest_ws_url: Option<String>,
    ingest_tcp_host: Option<String>,
    ingest_tcp_port: Option<u16>,
    capture_device: Option<String>,
    vad_threshold_db: f32,
    vad_hold_ms: u64,
}

impl RuntimeConfig {
    fn from_response(response: models::BridgeConfigResponse) -> Self {
        Self {
            assigned_input_id: response.assigned_input_id,
            ingest_ws_url: response.ingest_ws_url,
            ingest_tcp_host: response.ingest_tcp_host,
            ingest_tcp_port: response.ingest_tcp_port,
            capture_device: response.capture_device,
            vad_threshold_db: response.vad_threshold_db.unwrap_or(-45.0),
            vad_hold_ms: response.vad_hold_ms.unwrap_or(2000),
        }
    }

    fn update(&mut self, response: models::BridgeConfigResponse) -> Option<Self> {
        let mut changed = false;
        if response.assigned_input_id != self.assigned_input_id {
            self.assigned_input_id = response.assigned_input_id;
            changed = true;
        }
        if response.ingest_ws_url != self.ingest_ws_url {
            self.ingest_ws_url = response.ingest_ws_url;
            changed = true;
        }
        if response.ingest_tcp_host != self.ingest_tcp_host {
            self.ingest_tcp_host = response.ingest_tcp_host;
            changed = true;
        }
        if response.ingest_tcp_port != self.ingest_tcp_port {
            self.ingest_tcp_port = response.ingest_tcp_port;
            changed = true;
        }
        if response.capture_device != self.capture_device {
            self.capture_device = response.capture_device;
            changed = true;
        }
        if let Some(vad) = response.vad_threshold_db {
            self.vad_threshold_db = vad;
            changed = true;
        }
        if let Some(hold) = response.vad_hold_ms {
            self.vad_hold_ms = hold;
            changed = true;
        }
        if changed {
            Some(self.clone())
        } else {
            None
        }
    }

    fn is_ready(&self) -> bool {
        self.assigned_input_id.is_some()
            && self.capture_device.is_some()
            && (self.ingest_ws_url.is_some()
                || (self.ingest_tcp_host.is_some() && self.ingest_tcp_port.is_some()))
    }

    fn ingest_target(&self) -> Option<stream::IngestTarget> {
        if let Some(url) = &self.ingest_ws_url {
            return Some(stream::IngestTarget::Ws { url: url.clone() });
        }
        let host = self.ingest_tcp_host.clone()?;
        let port = self.ingest_tcp_port?;
        let header = self.assigned_input_id.clone()?;
        Some(stream::IngestTarget::Tcp { host, port, header })
    }

    fn ingest_label(&self) -> String {
        if let Some(url) = &self.ingest_ws_url {
            return url.clone();
        }
        match (&self.ingest_tcp_host, self.ingest_tcp_port) {
            (Some(host), Some(port)) => format!("{}:{}", host, port),
            _ => "unassigned".to_string(),
        }
    }
}

fn local_identity() -> Result<(String, String)> {
    let mut ip = None;
    if let Ok(ifaces) = get_if_addrs::get_if_addrs() {
        for iface in ifaces {
            if iface.is_loopback() {
                continue;
            }
            if let std::net::IpAddr::V4(addr) = iface.ip() {
                ip = Some(addr.to_string());
                break;
            }
        }
    }
    let ip = ip.unwrap_or_else(|| "0.0.0.0".to_string());
    let mac = mac_address::get_mac_address()
        .ok()
        .flatten()
        .map(|mac| mac.to_string())
        .unwrap_or_else(|| "00:00:00:00:00:00".to_string());
    Ok((ip, mac))
}

fn hash_capture_devices(devices: &[models::CaptureDeviceInfo]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    devices.hash(&mut hasher);
    hasher.finish()
}

struct Backoff {
    current: Duration,
}

impl Backoff {
    fn new() -> Self {
        Self {
            current: Duration::from_secs(1),
        }
    }

    fn reset(&mut self) {
        self.current = Duration::from_secs(1);
    }

    fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = std::cmp::min(self.current * 2, Duration::from_secs(30));
        delay
    }
}
