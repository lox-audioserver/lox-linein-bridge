use crate::stream::StatusHandle;
use serde::Serialize;
use std::fs;
use std::time::Duration;

const DEFAULT_HEALTH_PATH: &str = "/tmp/lox-linein-bridge.status.json";

#[derive(Debug, Serialize)]
pub struct HealthSnapshot {
    pub ts: String,
    pub state: String,
    pub device: String,
    pub ingest: String,
    pub last_error: Option<String>,
    pub bytes_sent_total: u64,
    pub last_chunk_ts: Option<String>,
}

pub fn spawn(status: StatusHandle) {
    let path = std::env::var("LOX_LINEIN_BRIDGE_HEALTH_PATH")
        .unwrap_or_else(|_| DEFAULT_HEALTH_PATH.to_string());
    tokio::spawn(async move {
        let mut last_write_ok = true;
        loop {
            let snapshot = status.health_snapshot();
            let payload = match serde_json::to_string_pretty(&snapshot) {
                Ok(payload) => payload,
                Err(err) => {
                    if last_write_ok {
                        tracing::warn!("health snapshot serialize failed: {}", err);
                        last_write_ok = false;
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            if let Err(err) = fs::write(&path, payload) {
                if last_write_ok {
                    tracing::warn!("health snapshot write failed: {}", err);
                    last_write_ok = false;
                }
            } else {
                last_write_ok = true;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}
