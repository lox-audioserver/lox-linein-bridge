# lox-linein-bridge

`lox-linein-bridge` is a tiny Linux CLI that captures ALSA audio and streams it to a lox-audioserver line-in ingest over TCP or WebSocket. It is designed for Raspberry Pi / SBC and keeps configuration fully automatic through server-side discovery and registration.

## Download

Prebuilt binaries are available for Linux (including Raspberry Pi / SBC). Targets:
- x86_64-unknown-linux-gnu
- aarch64-unknown-linux-gnu
- armv7-unknown-linux-gnueabihf
- arm-unknown-linux-gnueabihf (Pi 1 / Zero)

Raspberry Pi mapping:
- Pi 5 / 4 (64-bit OS): aarch64-unknown-linux-gnu
- Pi 3 (64-bit OS): aarch64-unknown-linux-gnu
- Pi 3 / 2 (32-bit OS): armv7-unknown-linux-gnueabihf
- Pi 1 / Zero: arm-unknown-linux-gnueabihf

Download the latest release for your device and place the `lox-linein-bridge` binary in `/usr/local/bin/`.

## Run (systemd)

```bash
lox-linein-bridge run
```

This is only meant for systemd. It discovers the server over mDNS, registers the bridge, and streams PCM audio based on server config.

## Install (systemd)

```bash
sudo lox-linein-bridge install
```

This writes the systemd unit, reloads systemd, and enables + starts the service.

mDNS discovery looks for `_loxaudio._tcp` and uses TXT fields:
- `api` (default `/api`)
- `linein_register` (default `/api/linein/bridges/register`)
- `linein_status` (default `/api/linein/bridges/{bridge_id}/status`)

## Voice activity detection (VAD)

To reduce bandwidth, the bridge uses a simple RMS-based gate. It only streams when audio is above the threshold, then holds the stream for a short time after the signal drops.

Tuning comes from the server's line-in ingest settings:
- `vad_threshold_db` (default: `-45.0` when unset)
- `vad_hold_ms` (default: `2000` when unset)

Example `GET /api/linein/{id}/ingest` response:
```json
{
  "linein_id": "linein-mke63267",
  "ingest_tcp_host": "192.168.1.209",
  "ingest_tcp_port": 7080,
  "vad_threshold_db": -45.0,
  "vad_hold_ms": 2000
}
```

## Configuration

The bridge writes:
- `/etc/lox-linein-bridge/config.toml` (preferred)
- `~/.config/lox-linein-bridge/config.toml` (fallback)

Example: `examples/config.toml`

Config fields:
- `bridge_id` (auto-generated if missing)
- `preferred_server_name` (optional mDNS TXT match)
- `preferred_server_mac` (optional mDNS TXT match)

## Systemd unit

The wizard writes `/etc/systemd/system/lox-linein-bridge.service`.

Example: `examples/lox-linein-bridge.service`

## Build (optional)

If you want to build from source on Raspberry Pi / SBC:

```bash
sudo apt-get install -y libasound2-dev pkg-config
cargo build --release
sudo cp target/release/lox-linein-bridge /usr/local/bin/
```

Then enable the service:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now lox-linein-bridge
```

Check service status:

```bash
systemctl status lox-linein-bridge
```
