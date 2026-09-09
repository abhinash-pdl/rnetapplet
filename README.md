# rnetapplet

A NetworkManager tray applet for wlroots-based Wayland compositors
(niri, Hyprland, Sway, river, …) that have no built-in network UI.

It runs as a single binary, sits in the tray as a StatusNotifierItem, and
opens a small popup for managing Wi-Fi — connect to networks, enter
passwords, turn Wi-Fi, airplane mode and the hotspot on or off, and even
Connect via a QR code. There is no separate settings window for network
connections; everything happens inside the popup.

## Features

- Nearby networks listed live, sorted by signal strength
- One-click connect to saved or open networks
- Inline password entry — the applet acts as its own NetworkManager
  SecretAgent, so a wrong password can be retried without a dialog
- Wi-Fi and airplane-mode toggles
- Hotspot creation (with name and password)
- QR-code connect from a webcam or an image file
- Settings shortcut that opens the system `nm-connection-editor`
- Tray icon follows the icon theme

## What it uses

| Part | Technology |
|---|---|
| Language | Rust (edition 2024) |
| GUI toolkit | GTK 4 |
| Popup layer | `wlr-layer-shell` (via gtk4-layer-shell) |
| Tray icon | StatusNotifierItem (via ksni) |
| NetworkManager API | D-Bus (via zbus / rusty-network-manager) |
| QR scanning | `rqrr` (decode) and `nokhwa` (webcam capture) |
| Async runtime | tokio, running on the main thread with GTK |

## Requirements

- A Wayland compositor that supports `wlr-layer-shell`
- NetworkManager running as the system network service
- A tray host with StatusNotifierItem support (e.g. the waybar `tray`
  module, swaync)
- Optional: a webcam for QR-code camera scanning

## Dependencies

Rust crates (fetched and built automatically by cargo):

`tokio`, `tokio-stream`, `futures`, `async-channel`, `anyhow`, `tracing`,
`tracing-subscriber`, `zbus`, `rusty-network-manager`, `ksni`, `gtk4`,
`gtk4-layer-shell`, `glib`, `gio`, `image`, `rqrr`, `nokhwa`.

System packages:

Arch Linux:

```sh
sudo pacman -S gtk4 gtk4-layer-shell networkmanager nm-connection-editor \
  rustup pkgconf gcc
```

Debian/Ubuntu (trixie or newer):

```sh
sudo apt install libgtk-4-dev libgtk4-layer-shell-dev network-manager \
  network-manager-gnome cargo pkg-config gcc
```

Fedora:

```sh
sudo dnf install gtk4-devel gtk4-layer-shell-devel NetworkManager \
  nm-connection-editor cargo gcc
```

## Installation

### Prebuilt binary

Download the latest binary tarball from the
[releases page](https://github.com/abhinash-pdl/rnetapplet/releases) and
install it:

```sh
curl -L -o rnetapplet.tar.gz \
  https://github.com/abhinash-pdl/rnetapplet/releases/download/v0.1.0/rnetapplet-0.1.0-x86_64-linux.tar.gz
tar xzf rnetapplet.tar.gz
cd rnetapplet-0.1.0-x86_64-linux
sudo install -Dm755 rnetapplet /usr/bin/rnetapplet
sudo install -Dm644 rnetapplet.desktop /usr/share/applications/rnetapplet.desktop
sudo install -Dm644 rnetapplet.service /usr/lib/systemd/user/rnetapplet.service
```

### cargo

```sh
cargo install --git https://github.com/abhinash-pdl/rnetapplet
```

The binary lands in `~/.cargo/bin/rnetapplet`.

### From source with make

```sh
git clone https://github.com/abhinash-pdl/rnetapplet
cd rnetapplet
make build-release
sudo make install    # installs to /usr/local by default, honours PREFIX/DESTDIR
```

`make install` installs:

- `/usr/bin/rnetapplet`
- `/usr/share/applications/rnetapplet.desktop`
- `/usr/lib/systemd/user/rnetapplet.service`

Uninstall with `sudo make uninstall`.

## Autostart

systemd (works with most compositors):

```sh
systemctl --user enable --now rnetapplet
```

niri (`~/.config/niri/config.kdl`):

```kdl
spawn-at-startup "/usr/bin/rnetapplet"
```

Hyprland (`~/.config/hypr/hyprland.conf`):

```ini
exec-once = /usr/bin/rnetapplet
```

Sway (`~/.config/sway/config`):

```ini
exec /usr/bin/rnetapplet
```

## Usage

- Left-click the tray icon to open or close the popup.
- Right-click the tray icon for the context menu (Open, Wi-Fi, Quit).
- Click a saved or open network to connect immediately.
- Click the chevron or the Connect button on a secured network to reveal an
  inline password field.

Command-line helpers:

```sh
rnetapplet --help            # list all options
rnetapplet --dump-aps        # scan and print networks, then exit
rnetapplet --wifi on|off     # toggle the Wi-Fi radio, then exit
rnetapplet --connect-saved "SSID"
```

## Troubleshooting

| Symptom | Likely cause / fix |
|---|---|
| No tray icon | No StatusNotifierItem host running; enable the waybar `tray` module. |
| Popup does not open | The compositor must support `wlr-layer-shell`. |
| Hotspot fails to start | The Wi-Fi driver does not support AP mode; check `journalctl -u NetworkManager`. |
| Missing icons | Run `rnetapplet --dump-aps` and look at the icon-theme audit. |

## Development

```sh
cargo check
cargo test
cargo run -- --dump-aps
RUST_LOG=rnetapplet=debug cargo run
```

The code is split into `nm_client` (NetworkManager over D-Bus),
`secret_agent` (the NetworkManager SecretAgent implementation), `tray`
(the StatusNotifierItem), `ui` (the GTK4 + wlr-layer-shell popup), and
`qr` / `qr_scan` (QR decode and camera scanning). GTK runs on the main
thread and the NetworkManager/tray work runs on a background tokio runtime;
the two are connected with channels.