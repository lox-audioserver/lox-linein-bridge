use crate::models::BridgeStatusRequest;
use anyhow::{Context, Result};
use futures_util::SinkExt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::info;

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;
const TRACK_GAP_MS: u64 = 2000;

#[derive(Clone)]
pub struct StatusHandle {
    inner: Arc<Mutex<StatusState>>,
}

struct StatusState {
    state: String,
    device: String,
    ingest: String,
    last_error: Option<String>,
    rate: Option<u32>,
    channels: Option<u16>,
    format: Option<String>,
    rms_db: Option<f32>,
    track_change: bool,
    bytes_sent_total: u64,
    last_chunk_ts: Option<String>,
}

impl StatusHandle {
    pub fn new(device: &str, ingest: &str) -> Self {
        Self {
            inner: Arc::new(Mutex::new(StatusState {
                state: "IDLE".to_string(),
                device: device.to_string(),
                ingest: ingest.to_string(),
                last_error: None,
                rate: None,
                channels: None,
                format: None,
                rms_db: None,
                track_change: false,
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

    pub fn set_device(&self, device: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.device = device.to_string();
        }
    }

    pub fn set_capture_info(&self, rate: u32, channels: u16, format: String) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.rate = Some(rate);
            inner.channels = Some(channels);
            inner.format = Some(format);
        }
    }

    pub fn set_rms_db(&self, rms_db: Option<f32>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.rms_db = rms_db;
        }
    }

    pub fn set_track_change(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.track_change = true;
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

    pub fn bridge_status(&self) -> BridgeStatusRequest {
        let mut inner = match self.inner.lock() {
            Ok(inner) => inner,
            Err(poisoned) => poisoned.into_inner(),
        };
        let track_change = if inner.track_change {
            inner.track_change = false;
            Some(true)
        } else {
            None
        };
        BridgeStatusRequest {
            state: inner.state.clone(),
            device: if inner.device.is_empty() {
                None
            } else {
                Some(inner.device.clone())
            },
            rate: inner.rate,
            channels: inner.channels,
            format: inner.format.clone(),
            rms_db: inner.rms_db,
            last_error: inner.last_error.clone(),
            track_change,
            capture_devices: None,
        }
    }

    pub fn set_ingest(&self, ingest: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.ingest = ingest.to_string();
        }
    }
}

pub enum IngestTarget {
    Tcp {
        host: String,
        port: u16,
        header: String,
    },
    Ws {
        url: String,
    },
}

pub struct StreamParams {
    pub ingest: IngestTarget,
    pub rx: mpsc::Receiver<Vec<u8>>,
    pub err_rx: mpsc::Receiver<String>,
    pub threshold_db: f32,
    pub hold_duration: Duration,
    pub vad_updates: Option<tokio::sync::watch::Receiver<(f32, Duration)>>,
    pub status: StatusHandle,
}

pub async fn stream_audio(mut params: StreamParams) -> Result<()> {
    match &params.ingest {
        IngestTarget::Tcp { .. } => stream_audio_tcp(&mut params).await,
        IngestTarget::Ws { .. } => stream_audio_ws(&mut params).await,
    }
}

async fn stream_audio_tcp(params: &mut StreamParams) -> Result<()> {
    let mut backoff = Backoff::new();
    let (host, port, header) = match &params.ingest {
        IngestTarget::Tcp { host, port, header } => (host.clone(), *port, header.clone()),
        IngestTarget::Ws { .. } => anyhow::bail!("invalid tcp ingest"),
    };
    let addr = format!("{}:{}", host, port);
    let mut gate = VadGate::new();
    let mut threshold_db = params.threshold_db;
    let mut hold_duration = params.hold_duration;
    let mut idle_since: Option<Instant> = None;
    let mut last_rate_log = Instant::now();
    let mut bytes_since_log: u64 = 0;

    let mut stream: Option<TcpStream> = None;
    loop {
        if stream.is_none() {
            params.status.set_state("RECONNECTING");
            match connect_tcp(&addr, &header).await {
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
                        let rms_db = rms_db_from_pcm_i16_le(&chunk);
                        params.status.set_rms_db(rms_db);
                        if let Some(rms_db) = rms_db {
                            let now = Instant::now();
                            let was_active = gate.active;
                            if rms_db >= threshold_db {
                                gate.set_active(now);
                            } else if gate.should_keep_active(now, hold_duration) {
                            } else {
                                gate.set_inactive();
                            }

                            if gate.active && !was_active {
                                if let Some(idle_start) = idle_since.take() {
                                    if now.duration_since(idle_start)
                                        >= Duration::from_millis(TRACK_GAP_MS)
                                    {
                                        params.status.set_track_change();
                                    }
                                }
                                info!("audio detected, streaming (rms_db={:.1})", rms_db);
                            } else if !gate.active && was_active {
                                idle_since = Some(now);
                                info!("silence detected, pausing stream (rms_db={:.1})", rms_db);
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
                                bytes_since_log += chunk.len() as u64;
                                if last_rate_log.elapsed() >= Duration::from_secs(5) {
                                    let secs = last_rate_log.elapsed().as_secs_f64();
                                    let bytes_per_sec = (bytes_since_log as f64 / secs).round();
                                    let est_rate = bytes_per_sec / 4.0;
                                    info!(
                                        "stream throughput: {} B/s (~{:.0} Hz)",
                                        bytes_per_sec, est_rate
                                    );
                                    bytes_since_log = 0;
                                    last_rate_log = Instant::now();
                                }
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
            _changed = async {
                match params.vad_updates.as_mut() {
                    Some(rx) => rx.changed().await.ok(),
                    None => None,
                }
            }, if params.vad_updates.is_some() => {
                if let Some(rx) = params.vad_updates.as_ref() {
                    let (next_threshold, next_hold) = *rx.borrow();
                    threshold_db = next_threshold;
                    hold_duration = next_hold;
                }
            }
        }
    }
}

async fn stream_audio_ws(params: &mut StreamParams) -> Result<()> {
    let mut backoff = Backoff::new();
    let url = match &params.ingest {
        IngestTarget::Ws { url } => url.clone(),
        IngestTarget::Tcp { .. } => anyhow::bail!("invalid ws ingest"),
    };
    let mut gate = VadGate::new();
    let mut threshold_db = params.threshold_db;
    let mut hold_duration = params.hold_duration;
    let mut idle_since: Option<Instant> = None;
    let mut last_rate_log = Instant::now();
    let mut bytes_since_log: u64 = 0;

    let mut stream = None;
    loop {
        if stream.is_none() {
            params.status.set_state("RECONNECTING");
            match connect_ws(&url).await {
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
                        let rms_db = rms_db_from_pcm_i16_le(&chunk);
                        params.status.set_rms_db(rms_db);
                        if let Some(rms_db) = rms_db {
                            let now = Instant::now();
                            let was_active = gate.active;
                            if rms_db >= threshold_db {
                                gate.set_active(now);
                            } else if gate.should_keep_active(now, hold_duration) {
                            } else {
                                gate.set_inactive();
                            }

                            if gate.active && !was_active {
                                if let Some(idle_start) = idle_since.take() {
                                    if now.duration_since(idle_start)
                                        >= Duration::from_millis(TRACK_GAP_MS)
                                    {
                                        params.status.set_track_change();
                                    }
                                }
                                info!("audio detected, streaming (rms_db={:.1})", rms_db);
                            } else if !gate.active && was_active {
                                idle_since = Some(now);
                                info!("silence detected, pausing stream (rms_db={:.1})", rms_db);
                            }
                        }

                        if !gate.active {
                            params.status.set_state("IDLE");
                            continue;
                        }

                        if let Some(writer) = stream.as_mut() {
                            let chunk_len = chunk.len();
                            if let Err(err) = writer.send(Message::Binary(chunk)).await {
                                params.status.set_last_error(Some(err.to_string()));
                                stream = None;
                            } else {
                                params.status.set_state("STREAMING");
                                params.status.record_bytes(chunk_len);
                                bytes_since_log += chunk_len as u64;
                                if last_rate_log.elapsed() >= Duration::from_secs(5) {
                                    let secs = last_rate_log.elapsed().as_secs_f64();
                                    let bytes_per_sec = (bytes_since_log as f64 / secs).round();
                                    let est_rate = bytes_per_sec / 4.0;
                                    info!(
                                        "stream throughput: {} B/s (~{:.0} Hz)",
                                        bytes_per_sec, est_rate
                                    );
                                    bytes_since_log = 0;
                                    last_rate_log = Instant::now();
                                }
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
            _changed = async {
                match params.vad_updates.as_mut() {
                    Some(rx) => rx.changed().await.ok(),
                    None => None,
                }
            }, if params.vad_updates.is_some() => {
                if let Some(rx) = params.vad_updates.as_ref() {
                    let (next_threshold, next_hold) = *rx.borrow();
                    threshold_db = next_threshold;
                    hold_duration = next_hold;
                }
            }
        }
    }
}

async fn connect_tcp(addr: &str, header: &str) -> Result<TcpStream> {
    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect to {}", addr))?;
    stream.set_nodelay(true).context("set TCP nodelay")?;
    let header_line = format!("{}\n", header);
    stream
        .write_all(header_line.as_bytes())
        .await
        .context("send input id")?;
    Ok(stream)
}

async fn connect_ws(url: &str) -> Result<WsStream> {
    let (stream, _) = connect_async(url)
        .await
        .with_context(|| format!("connect ws {}", url))?;
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
