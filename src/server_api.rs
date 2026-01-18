use crate::models::{IngestTarget, LineIn, StatusSnapshot};
use anyhow::{Context, Result};
use reqwest::Client;
use url::Url;

#[derive(Clone)]
pub struct ServerApi {
    base_url: String,
    client: Client,
}

impl ServerApi {
    pub fn new(server_url: &str) -> Result<Self> {
        let url = Url::parse(server_url).context("invalid server URL")?;
        let mut base_url = url.to_string();
        while base_url.ends_with('/') {
            base_url.pop();
        }

        Ok(Self {
            base_url,
            client: Client::new(),
        })
    }

    pub fn base_host(&self) -> Result<String> {
        let url = Url::parse(&self.base_url).context("invalid server URL")?;
        url.host_str()
            .map(|s| s.to_string())
            .context("server URL missing host")
    }

    pub async fn discover_lineins(&self) -> Result<Vec<LineIn>> {
        let url = format!("{}/api/linein", self.base_url);
        let response = self
            .client
            .get(url)
            .send()
            .await
            .context("request line-ins")?
            .error_for_status()
            .context("line-ins response status")?;
        let lineins = response
            .json::<Vec<LineIn>>()
            .await
            .context("parse line-ins")?;
        Ok(lineins)
    }

    pub async fn get_ingest(&self, linein_id: &str) -> Result<IngestTarget> {
        let url = format!("{}/api/linein/{}/ingest", self.base_url, linein_id);
        let response = self
            .client
            .get(url)
            .send()
            .await
            .context("request ingest target")?
            .error_for_status()
            .context("ingest response status")?;
        let ingest = response
            .json::<IngestTarget>()
            .await
            .context("parse ingest")?;
        Ok(ingest)
    }

    pub async fn post_status(&self, linein_id: &str, snapshot: &StatusSnapshot) -> Result<()> {
        let url = format!("{}/api/linein/{}/bridge-status", self.base_url, linein_id);
        self.client
            .post(url)
            .json(snapshot)
            .send()
            .await
            .context("post status")?
            .error_for_status()
            .context("status response status")?;
        Ok(())
    }
}
