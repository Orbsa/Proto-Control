# Proto-Control

A Linux daemon that brings your **Melbourne Instruments Roto-Control** device to life — mapping PipeWire audio streams to knobs/buttons, and putting Discord or TeamSpeak voice channel members on dedicated pages for per-user volume and mute control.

## Features

- **PipeWire integration** — each audio output stream gets its own knob (volume) and button (mute). Stream names, colors, and track titles update live.
- **Discord voice** — per-user volume/mute on page 2, sorted by activity. Requires a Discord application with local RPC enabled.
- **TeamSpeak voice** — per-user volume/mute on page 3 via a companion TS3 plugin.
- **Settings GUI** — system-tray icon opens a GUI for renaming streams, assigning colors, managing user priorities, and enabling/disabling integrations — no TOML editing required for day-to-day use.
- **Live config reload** — changes saved in the GUI apply within ~1 second without restarting the daemon.
- **Volume memory** — remembered per-app so volumes are restored when a stream restarts.
- **MPRIS track titles** — optionally pull the current track title from `playerctl` and show it on the button display.

## Requirements

- Linux with **PipeWire** running
- `pw-dump` and `wpctl` (shipped with PipeWire/WirePlumber)
- `pactl` (for stream change events; ships with PulseAudio/PipeWire-pulse)
- Optionally `playerctl` for MPRIS track title display
- The Melbourne Instruments Roto-Control device connected via USB

## Installation

### Pre-built binary (recommended)

Download the latest release from the [Releases page](../../releases):

- `proto-control-*-linux-x86_64` — dynamically linked against glibc; works on Ubuntu, Fedora, Arch, etc.
- `proto-control-*-linux-x86_64-nix` — built with Nix; best on NixOS (requires `/nix/store`).

Make the binary executable and place it in your `PATH`:

```sh
chmod +x proto-control-*-linux-x86_64
sudo install -m755 proto-control-*-linux-x86_64 /usr/local/bin/proto-control
```

### NixOS / nix-shell

```sh
nix run github:your-org/proto-control
```

Or add to your flake inputs and use `packages.default`.

### Build from source

```sh
# With Nix (recommended — handles all deps automatically)
nix build

# Without Nix — install deps first:
# Ubuntu/Debian: sudo apt install pkg-config libudev-dev libasound2-dev \
#   libwayland-dev libxkbcommon-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev
cargo build --release --manifest-path daemon/Cargo.toml
```

## Configuration

Proto-Control reads `~/.config/proto-control/config.toml` on startup and reloads it automatically when the file changes.

### Minimal config (PipeWire only)

No config file needed — PipeWire streams are detected and displayed automatically.

### Full config example

```toml
# Disable PipeWire page (default: true)
pipewire_enabled = true

# ── Stream overrides ──────────────────────────────────────────────────────
# Rename a stream and assign a knob color
[[streams]]
binary = "zen"
name   = "Zen"
color  = 9          # blue (see color index table below)

# Enable MPRIS track title (shown on the button display below the knob)
[[streams]]
binary       = "tidal-hifi"
name         = "Tidal"
mpris_player = "tidal-hifi"   # pull current track from playerctl
color        = 9              # blue
accent_color = 9              # button accent color when track title is shown

# Hide a stream entirely
[[streams]]
binary  = "some-background-app"
ignored = true

# ── Discord integration ───────────────────────────────────────────────────
[discord]
client_id     = "YOUR_DISCORD_CLIENT_ID"
client_secret = "YOUR_DISCORD_CLIENT_SECRET"
enabled       = true

[[discord_users]]
name     = "Alice"
color    = 0       # pink
priority = 1       # appears first

[[discord_users]]
name     = "Bob"
priority = 2

# ── TeamSpeak integration ─────────────────────────────────────────────────
[teamspeak]
# socket_path is auto-detected (Flatpak or native install).
# Override only if your plugin socket is in a non-standard location:
# socket_path = "/tmp/proto-control-ts3.sock"
enabled = true

[[teamspeak_users]]
name     = "Charlie"
color    = 7       # teal
priority = 1
```

### Color index reference

| Index | Color  |
|-------|--------|
| 0     | Pink   |
| 1     | Orange |
| 3     | Yellow |
| 5     | Green  |
| 7     | Teal   |
| 9     | Blue   |
| 11    | Purple |
| 14    | Red    |
| 70    | Off    |
| `null` / omitted | Hash-based random |

### Discord setup

1. Go to the [Discord Developer Portal](https://discord.com/developers/applications) and create a new application.
2. Under **OAuth2**, note the **Client ID** and **Client Secret**.
3. Add `http://127.0.0.1` as a redirect URI.
4. Add these to `config.toml` under `[discord]`.

On first run Proto-Control will open a browser tab for OAuth authorization. The token is cached automatically.

### TeamSpeak setup

Proto-Control communicates with TeamSpeak via a companion plugin that creates a Unix socket.

1. Download `proto-control-ts3-plugin_linux_amd64.so` from the [Releases page](../../releases).
2. Copy it into your TS3 plugin directory:
   - **Flatpak**: `~/.var/app/com.teamspeak.TeamSpeak3/.ts3client/plugins/`
   - **Native**: `~/.ts3client/plugins/`
3. Restart TeamSpeak and enable the plugin in **Tools → Plugins**.
4. Add `[teamspeak]` to `config.toml` and restart the daemon.

The plugin auto-detects which install location to use for the socket. You can override with `socket_path` in `config.toml` if needed.

## Usage

```sh
# Start the daemon (connects to the device, starts all enabled integrations)
proto-control

# Open the settings GUI directly
proto-control --settings
```

The system tray icon provides quick access to Settings and Quit.

### Settings GUI

- **PipeWire tab** — rename streams, assign knob/accent colors, enable MPRIS track titles, ignore streams.
- **Discord tab** — set per-user colors and sort priority. Active voice members can be added from the dropdown.
- **TeamSpeak tab** — same as Discord.
- **Enabled checkbox** on each tab header — disables the integration without removing credentials.

Changes take effect within ~1 second of saving (no restart needed).

## Autostart

### systemd user service

```ini
# ~/.config/systemd/user/proto-control.service
[Unit]
Description=Proto-Control daemon
After=pipewire.service

[Service]
ExecStart=/usr/local/bin/proto-control
Restart=on-failure

[Install]
WantedBy=default.target
```

```sh
systemctl --user enable --now proto-control
```

### NixOS home-manager

```nix
systemd.user.services.proto-control = {
  Unit.Description = "Proto-Control daemon";
  Unit.After = [ "pipewire.service" ];
  Service.ExecStart = "${pkgs.proto-control}/bin/proto-control";
  Service.Restart = "on-failure";
  Install.WantedBy = [ "default.target" ];
};
```

## Versioning

The version in `daemon/Cargo.toml` drives releases. Bump it and push to `master` — the CI workflow creates a GitHub Release and uploads binaries automatically when it sees a new version tag.

## License

MIT
