use crate::config;
use anyhow::{Context, Result};
use std::fs;
use std::process::Command;

const SYSTEMD_UNIT_PATH: &str = "/etc/systemd/system/lox-linein-bridge.service";

pub async fn run_install() -> Result<()> {
    let (_config, config_path) = config::load_or_create_config()?;
    println!("Config: {}", config_path.display());

    let unit = systemd_unit();
    fs::write(SYSTEMD_UNIT_PATH, unit).context("write systemd unit")?;
    println!("Wrote systemd unit: {}", SYSTEMD_UNIT_PATH);

    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "--now", "lox-linein-bridge"])?;
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

fn run_systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .args(args)
        .status()
        .with_context(|| format!("run systemctl {}", args.join(" ")))?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("systemctl {} failed", args.join(" "));
    }
}
