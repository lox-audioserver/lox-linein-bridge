mod alsa_silence;
mod audio;
mod config;
mod health;
mod install;
mod models;
mod server_api;
mod stream;
mod timestamp;

use anyhow::Result;
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
        "install" => {
            if args.len() != 4 || args[2] != "--server" {
                print_usage();
                anyhow::bail!("expected `install --server <url>`");
            }
            install::run_install(&args[3]).await
        }
        "run" => run().await,
        _ => {
            print_usage();
            anyhow::bail!("unknown command");
        }
    }
}

async fn run() -> Result<()> {
    let (config, path) = config::load_config()?;
    info!("loaded config from {}", path.display());

    let api = server_api::ServerApi::new(&config.server_url)?;

    let ingest = match api.get_ingest(&config.linein_id).await {
        Ok(ingest) => ingest,
        Err(err) => {
            warn!("ingest lookup failed: {}", err);
            let host = api.base_host()?;
            models::IngestTarget {
                ingest_tcp_host: host,
                ingest_tcp_port: 7080,
                vad_threshold_db: None,
                vad_hold_ms: None,
            }
        }
    };

    let ingest_addr = format!("{}:{}", ingest.ingest_tcp_host, ingest.ingest_tcp_port);
    let vad_threshold_db = ingest.vad_threshold_db.unwrap_or(-45.0);
    let vad_hold_ms = ingest.vad_hold_ms.unwrap_or(2000);
    info!("capture device: {}", config.capture_device);
    info!(
        "ingest target {} (vad_threshold_db={}, vad_hold_ms={})",
        ingest_addr, vad_threshold_db, vad_hold_ms
    );
    let status = stream::StatusHandle::new(&config.capture_device, &ingest_addr);
    health::spawn(status.clone());

    let status_api = api.clone();
    let linein_id = config.linein_id.clone();
    let status_handle = status.clone();
    tokio::spawn(async move {
        loop {
            let snapshot = status_handle.snapshot();
            if let Err(err) = status_api.post_status(&linein_id, &snapshot).await {
                tracing::debug!("status post failed: {}", err);
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    let mut backoff = Backoff::new();
    loop {
        match audio::start_capture(&config.capture_device) {
            Ok(session) => {
                backoff.reset();
                let audio::CaptureSession {
                    receiver,
                    error_receiver,
                    stream,
                } = session;
                let _stream_guard = stream;
                let params = stream::StreamParams {
                    linein_id: &config.linein_id,
                    ingest_host: &ingest.ingest_tcp_host,
                    ingest_port: ingest.ingest_tcp_port,
                    rx: receiver,
                    err_rx: error_receiver,
                    threshold_db: vad_threshold_db,
                    hold_duration: std::time::Duration::from_millis(vad_hold_ms),
                    status: status.clone(),
                };

                if let Err(err) = stream::stream_audio(params).await {
                    status.set_state("ERROR");
                    status.set_last_error(Some(err.to_string()));
                    warn!("streaming stopped: {}", err);
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
    eprintln!("  lox-linein-bridge install --server http://<lox-host>:7090");
    eprintln!("  lox-linein-bridge run");
    eprintln!("  lox-linein-bridge --help");
    eprintln!("  lox-linein-bridge --version");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  lox-linein-bridge install --server http://192.168.1.209:7090");
    eprintln!("  lox-linein-bridge run");
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
