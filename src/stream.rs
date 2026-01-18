use crate::models::StatusSnapshot;
use anyhow::{Context, Result};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::info;

#[derive(Clone)]
pub struct StatusHandle {
    inner: Arc<Mutex<StatusState>>,
}

struct StatusState {
    state: String,
    device: String,
    ingest: String,
    last_error: Option<String>,
    bytes_sent_total: u64,
    last_chunk_ts: Option<String>,
}

impl StatusHandle {
    pub fn new(device: &str, ingest: &str) -> Self {
        Self {
            inner: Arc::new(Mutex::new(StatusState {
                state: "RECONNECTING".to_string(),
                device: device.to_string(),
                ingest: ingest.to_string(),
                last_error: None,
                bytes_sent_total: 0,
                last_chunk_ts: None,
            })),
        }
    }

    pub fn set_state(&self, state: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.state = state.to_string();
        }
    }

    pub fn set_last_error(&self, error: Option<String>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.last_error = error;
        }
    }

    pub fn snapshot(&self) -> StatusSnapshot {
        let inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(poisoned) => poisoned.into_inner(),
        };
        StatusSnapshot {
            ts: crate::timestamp::now_rfc3339(),
            state: inner.state.clone(),
            device: inner.device.clone(),
            ingest: inner.ingest.clone(),
            last_error: inner.last_error.clone(),
        }
    }

    pub fn record_bytes(&self, bytes: usize) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.bytes_sent_total = inner.bytes_sent_total.saturating_add(bytes as u64);
            inner.last_chunk_ts = Some(crate::timestamp::now_rfc3339());
        }
    }

    pub fn health_snapshot(&self) -> crate::health::HealthSnapshot {
        let inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(poisoned) => poisoned.into_inner(),
        };
        crate::health::HealthSnapshot {
            ts: crate::timestamp::now_rfc3339(),
            state: inner.state.clone(),
            device: inner.device.clone(),
            ingest: inner.ingest.clone(),
            last_error: inner.last_error.clone(),
            bytes_sent_total: inner.bytes_sent_total,
            last_chunk_ts: inner.last_chunk_ts.clone(),
        }
    }
}

pub struct StreamParams<'a> {
    pub linein_id: &'a str,
    pub ingest_host: &'a str,
    pub ingest_port: u16,
    pub rx: mpsc::Receiver<Vec<u8>>,
    pub err_rx: mpsc::Receiver<String>,
    pub threshold_db: f32,
    pub hold_duration: Duration,
    pub status: StatusHandle,
}

pub async fn stream_audio(mut params: StreamParams<'_>) -> Result<()> {
    let mut backoff = Backoff::new();
    let addr = format!("{}:{}", params.ingest_host, params.ingest_port);
    let mut gate = VadGate::new();

    let mut stream: Option<TcpStream> = None;
    loop {
        if stream.is_none() {
            params.status.set_state("RECONNECTING");
            match connect(&addr, params.linein_id).await {
                Ok(connected) => {
                    stream = Some(connected);
                    params.status.set_state("STREAMING");
                    params.status.set_last_error(None);
                    backoff.reset();
                }
                Err(err) => {
                    params.status.set_last_error(Some(err.to_string()));
                    tokio::time::sleep(backoff.next_delay()).await;
                    continue;
                }
            }
        }

        tokio::select! {
            maybe_chunk = params.rx.recv() => {
                match maybe_chunk {
                    Some(chunk) => {
                        if let Some(rms_db) = rms_db_from_pcm_i16_le(&chunk) {
                            let now = Instant::now();
                            let was_active = gate.active;
                            if rms_db >= params.threshold_db {
                                gate.set_active(now);
                            } else if gate.should_keep_active(now, params.hold_duration) {
                                gate.touch(now);
                            } else {
                                gate.set_inactive();
                            }

                            if gate.active && !was_active {
                                info!("vad: active (rms_db={:.1})", rms_db);
                            } else if !gate.active && was_active {
                                info!("vad: idle (rms_db={:.1})", rms_db);
                            }
                        }

                        if !gate.active {
                            params.status.set_state("IDLE");
                            continue;
                        }

                        if let Some(writer) = stream.as_mut() {
                            if let Err(err) = writer.write_all(&chunk).await {
                                params.status.set_last_error(Some(err.to_string()));
                                stream = None;
                            } else {
                                params.status.set_state("STREAMING");
                                params.status.record_bytes(chunk.len());
                            }
                        }
                    }
                    None => {
                        return Err(anyhow::anyhow!("audio capture channel closed"));
                    }
                }
            }
            maybe_err = params.err_rx.recv() => {
                let message = match maybe_err {
                    Some(message) => message,
                    None => "audio capture error channel closed".to_string(),
                };
                params.status.set_last_error(Some(message.clone()));
                return Err(anyhow::anyhow!(message));
            }
        }
    }
}

async fn connect(addr: &str, linein_id: &str) -> Result<TcpStream> {
    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect to {}", addr))?;
    stream.set_nodelay(true).context("set TCP nodelay")?;
    let header = format!("{}\n", linein_id);
    stream
        .write_all(header.as_bytes())
        .await
        .context("send line-in id")?;
    Ok(stream)
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

struct VadGate {
    active: bool,
    last_active: Option<Instant>,
}

impl VadGate {
    fn new() -> Self {
        Self {
            active: false,
            last_active: None,
        }
    }

    fn set_active(&mut self, now: Instant) {
        self.active = true;
        self.last_active = Some(now);
    }

    fn touch(&mut self, now: Instant) {
        self.last_active = Some(now);
    }

    fn set_inactive(&mut self) {
        self.active = false;
    }

    fn should_keep_active(&self, now: Instant, hold: Duration) -> bool {
        match self.last_active {
            Some(ts) => now.duration_since(ts) <= hold,
            None => false,
        }
    }
}

fn rms_db_from_pcm_i16_le(bytes: &[u8]) -> Option<f32> {
    let mut sum = 0f64;
    let mut count = 0u64;
    for chunk in bytes.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
        let normalized = sample as f64 / i16::MAX as f64;
        sum += normalized * normalized;
        count += 1;
    }
    if count == 0 {
        return None;
    }
    let mean = sum / count as f64;
    let rms = mean.sqrt();
    let db = if rms <= 0.0 {
        -100.0
    } else {
        20.0 * rms.log10()
    };
    Some(db as f32)
}
