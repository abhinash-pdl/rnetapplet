# rnetapplet

A NetworkManager tray applet for wlroots-based Wayland compositors
(niri, Hyprland, Sway, river, …) that have no built-in network UI.

It runs as a single binary, sits in the tray as a StatusNotifierItem, and
opens a small popup for managing Wi-Fi — connect to networks, enter
passwords, turn Wi-Fi, airplane mode and the hotspot on or off, and even
Connect via a QR code. There is no separate settings window for network
connections; everything happens inside the popup.

## Features

- Nearby networks listed live with signal bands (2.4G/5G/6G), sorted by signal strength
- One-click connect to saved or open networks; saved networks stay
  listed with connect/forget even out of range
- Inline password entry — the applet acts as its own NetworkManager
  SecretAgent, so a wrong password can be retried without a dialog
- Malformed passwords are rejected before they reach NetworkManager, and
  if a connect attempt fails, the previous network is re-activated
  automatically
- Connected details: IPv4, gateway, DNS, band, MAC, live up/down speeds
- Wi-Fi and airplane-mode toggles, wired connection status
- Hotspot creation (with name and password)
- QR-code connect from a webcam or an image file
- Desktop notifications on connect/disconnect
- Settings shortcut that opens the system `nm-connection-editor`
- Tray icon follows the icon theme
- Saved VPN connections listed with connect/disconnect
- Manual rescan button plus automatic live updates

## What it uses

| Part | Technology |
|---|---|
| Language | Rust (edition 2024) |
| GUI toolkit | GTK 4 |
| Popup layer | `wlr-layer-shell` (via gtk4-layer-shell) |
| Tray icon | StatusNotifierItem (via ksni) |
| NetworkManager API | D-Bus (via zbus / rusty-network-manager) |
| QR scanning | `rqrr` (decode) and `nokhwa` (webcam capture) |
| Async runtime | tokio (background multi-thread runtime) |

## Requirements

- A Wayland compositor that supports `wlr-layer-shell`
- NetworkManager running as the system network service
- A tray host with StatusNotifierItem support (e.g. the waybar `tray`
  module, swaync)
- Optional: a webcam for QR-code camera scanning
- Recommended: `xdg-desktop-portal` (plus the compositor's portal
  implementation) so the theme, colors and scale are known before the
  popup first renders

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
  https://github.com/abhinash-pdl/rnetapplet/releases/download/v0.1.3/rnetapplet-0.1.3-x86_64-linux.tar.gz
echo "23efc3c3d250a61a4fd38ddbf0dc72a15454fe6d0bb8b3b1599e26a7cae32dfc  rnetapplet.tar.gz" | sha256sum -c -
tar xzf rnetapplet.tar.gz
cd rnetapplet-0.1.3-x86_64-linux
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

Desktop file (any compositor or desktop; waits for the session bus,
settings portal, display and GPU before launching, so the theme and
colors always match):

```sh
mkdir -p ~/.config/autostart
cp /usr/share/applications/rnetapplet.desktop ~/.config/autostart/
```

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

## Running under systemd

The shipped unit (`rnetapplet.service`) runs per-user, stops with the
graphical session (`PartOf=graphical-session.target`), and restarts on
failure (`Restart=on-failure`). Manage it with systemd:

```sh
systemctl --user enable --now rnetapplet   # start now and at login
systemctl --user restart rnetapplet        # after updating or reinstalling
systemctl --user status rnetapplet
journalctl --user -u rnetapplet -f          # follow its logs
```

The user manager starts with a minimal environment: variables your desktop
session exports — `GTK_THEME`, `GDK_BACKEND`, GPU/Mesa overrides, and so on —
are not carried over, which can make the popup render in a default theme, at
a different scale, or through a software-GL path. Give the service the same
environment as your session, per-service, with a drop-in:

```sh
mkdir -p ~/.config/systemd/user/rnetapplet.service.d
cat > ~/.config/systemd/user/rnetapplet.service.d/10-desktop-env.conf <<'EOF'
[Service]
Environment=GTK_THEME=Materia-dark
Environment=GDK_BACKEND=wayland,x11
Environment=MESA_LOADER_DRIVER_OVERRIDE=iris
EOF
systemctl --user daemon-reload
systemctl --user restart rnetapplet
```

Set each `Environment=` line to match your own session. The applet itself
defaults `GDK_BACKEND=wayland,x11` when the variable is unset, so the main
things to mirror are the theme and any GPU/driver overrides. As an
alternative to a drop-in, import the session environment into the user
manager before starting the service (niri:
`spawn-at-startup "systemctl --user import-environment WAYLAND_DISPLAY DISPLAY XDG_CURRENT_DESKTOP"`,
Hyprland: the same command in `exec-once`).

## Usage

- Left-click the tray icon to open or close the popup.
- Click anywhere outside the popup to dismiss it.
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
| Popup theme differs when autostarted by systemd | The user manager did not inherit the compositor's environment. See [Running under systemd](#running-under-systemd) for the per-service drop-in or `import-environment` fix. |

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