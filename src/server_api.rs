use crate::models::{BridgeConfigResponse, BridgeRegisterRequest, BridgeStatusRequest};
use anyhow::{Context, Result};
use reqwest::Client;

#[derive(Clone)]
pub struct ServerApi {
    base_url: String,
    register_path: String,
    status_path: String,
    client: Client,
}

impl ServerApi {
    pub fn new(base_url: &str, register_path: &str, status_path: &str) -> Result<Self> {
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            register_path: register_path.to_string(),
            status_path: status_path.to_string(),
            client: Client::new(),
        })
    }

    pub async fn register_bridge(
        &self,
        request: &BridgeRegisterRequest,
    ) -> Result<BridgeConfigResponse> {
        let url = format!("{}{}", self.base_url, self.register_path);
        let response = self
            .client
            .post(url)
            .json(request)
            .send()
            .await
            .context("register bridge")?
            .error_for_status()
            .context("register response status")?;
        let config = response
            .json::<BridgeConfigResponse>()
            .await
            .context("parse register response")?;
        Ok(config)
    }

    pub async fn post_status(
        &self,
        bridge_id: &str,
        status: &BridgeStatusRequest,
    ) -> Result<BridgeConfigResponse> {
        let url = format!(
            "{}{}",
            self.base_url,
            self.status_path.replace("{bridge_id}", bridge_id)
        );
        let response = self
            .client
            .post(url)
            .json(status)
            .send()
            .await
            .context("post status")?
            .error_for_status()
            .context("status response status")?;
        let config = response
            .json::<BridgeConfigResponse>()
            .await
            .context("parse status response")?;
        Ok(config)
    }
}
