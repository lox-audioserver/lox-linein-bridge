use crate::audio;
use crate::config::{self, Config};
use crate::server_api::ServerApi;
use anyhow::{Context, Result};
use dialoguer::Select;
use std::fs;
use url::Url;

const SYSTEMD_UNIT_PATH: &str = "/etc/systemd/system/lox-linein-bridge.service";

pub async fn run_install(server_url: &str) -> Result<()> {
    let url = Url::parse(server_url).context("invalid server URL")?;
    if url.scheme() != "http" && url.scheme() != "https" {
        anyhow::bail!("server URL must start with http:// or https://");
    }

    let api = ServerApi::new(server_url)?;
    let lineins = api.discover_lineins().await?;
    if lineins.is_empty() {
        anyhow::bail!("no line-ins found on server");
    }

    println!("Select a line-in:");
    let items: Vec<String> = lineins
        .iter()
        .map(|linein| format!("{} (id: {})", linein.name, linein.id))
        .collect();
    let linein_index = Select::new().items(&items).default(0).interact()?;
    let linein = &lineins[linein_index];

    let devices = audio::list_input_devices()?;
    if devices.is_empty() {
        anyhow::bail!("no ALSA capture devices found");
    }

    let capture_device = if devices.len() == 1 {
        println!("Using capture device: {}", devices[0].name);
        devices[0].name.clone()
    } else {
        println!("Select a capture device:");
        let device_names: Vec<String> = devices.iter().map(|d| d.name.clone()).collect();
        let idx = Select::new().items(&device_names).default(0).interact()?;
        devices[idx].name.clone()
    };

    let config = Config {
        server_url: server_url.to_string(),
        linein_id: linein.id.clone(),
        capture_device,
    };
    let config_path = config::write_config(&config)?;
    println!("Wrote config: {}", config_path.display());

    let unit = systemd_unit();
    fs::write(SYSTEMD_UNIT_PATH, unit).context("write systemd unit")?;
    println!("Wrote systemd unit: {}", SYSTEMD_UNIT_PATH);

    println!();
    println!("Next steps:");
    println!("  systemctl daemon-reload");
    println!("  systemctl enable --now lox-linein-bridge");
    Ok(())
}

fn systemd_unit() -> String {
    [
        "[Unit]",
        "Description=Lox Line-in Bridge",
        "After=network-online.target",
        "",
        "[Service]",
        "Type=simple",
        "ExecStart=/usr/local/bin/lox-linein-bridge run",
        "Restart=always",
        "RestartSec=2",
        "",
        "[Install]",
        "WantedBy=multi-user.target",
        "",
    ]
    .join("\n")
}
