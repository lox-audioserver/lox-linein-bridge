use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct LineIn {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct IngestTarget {
    pub ingest_tcp_host: String,
    pub ingest_tcp_port: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusSnapshot {
    pub ts: String,
    pub state: String,
    pub device: String,
    pub ingest: String,
    pub last_error: Option<String>,
}
