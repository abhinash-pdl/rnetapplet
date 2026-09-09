mod nm_client;
mod qr;
mod qr_scan;
mod secret_agent;
mod state;
mod tray;
mod ui;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use gtk4::gio::prelude::FileExt as _;
use tracing::info;

const HELP: &str = "rnetapplet — NetworkManager tray applet for wlroots compositors

USAGE:
    rnetapplet [FLAGS] [OPTIONS]

FLAGS:
    --dump-aps            Print visible access points and exit
    --camera-test         Probe the camera + QR decode path and exit
    --hotspot-test        Create a hotspot, observe, stop it, restore Wi-Fi, exit
    -h, --help            Print this help
    -V, --version         Print version

OPTIONS:
    --connect-saved <SSID>  Activate a saved profile and exit
    --wifi <on|off>         Set the Wi-Fi radio and exit

ENV:
    RUST_LOG                Tracing filter (default: info), e.g. RUST_LOG=rnetapplet=debug
";

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "error".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("rnetapplet {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let dump = args.iter().any(|a| a == "--dump-aps");
    let connect_saved = take_value(&args, "--connect-saved");
    let set_wifi = take_value(&args, "--wifi");
    let camera_test = args.iter().any(|a| a == "--camera-test");
    let hotspot_test = args.iter().any(|a| a == "--hotspot-test");

    if let Some(v) = &set_wifi {
        if v != "on" && v != "off" {
            anyhow::bail!("--wifi expects on|off, got {v:?}");
        }
    }

    if dump || connect_saved.is_some() || set_wifi.is_some() || camera_test || hotspot_test {
        if camera_test {

            return qr_scan::camera_self_test();
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        if hotspot_test {
            return rt.block_on(hotspot_test_run());
        }
        if let Some(ssid) = connect_saved {
            return rt.block_on(connect_saved_test(&ssid));
        }
        if let Some(v) = set_wifi {
            return rt.block_on(wifi_test(&v));
        }
        return rt.block_on(dump_aps());
    }

    if std::env::var_os("GDK_BACKEND").is_none() {
        unsafe { std::env::set_var("GDK_BACKEND", "wayland,x11") };
    }

    wait_for_compositor(std::time::Duration::from_secs(20))?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()?;

    let initial = rt.block_on(async {
        let client = nm_client::NmClient::connect().await?;
        info!(version = %client.version().await?, "connected to NetworkManager");
        anyhow::Ok(client.refresh_model().await.unwrap_or_default())
    })?;

    let (model_feed_tx, model_feed_rx) = async_channel::bounded::<state::UiEvent>(32);
    let (toggle_tx, toggle_rx) = async_channel::bounded::<Option<(i32, i32)>>(8);
    let (quit_tx, quit_rx) = async_channel::bounded::<()>(2);

    let (tray_ev_tx, tray_ev_rx) = async_channel::bounded::<tray::TrayEvent>(16);

    rt.spawn(backend_main(
        model_feed_tx,
        toggle_tx,
        quit_tx.clone(),
        tray_ev_tx.clone(),
        tray_ev_rx,
    ));

    info!("starting layer-shell popup (main thread)");
    ui::run(initial, model_feed_rx, toggle_rx, quit_rx, tray_ev_tx);

    rt.shutdown_background();
    Ok(())
}

async fn connect_saved_test(ssid: &str) -> Result<()> {
    let client = nm_client::NmClient::connect().await?;
    info!(version = %client.version().await?, "connected to NetworkManager");
    client.connect_saved(ssid).await?;
    for i in 0..4 {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let active = client.active_ssid().await.unwrap_or(None);
        println!("t+{}s active={}", (i + 1) * 3, active.as_deref().unwrap_or("(none)"));
    }
    Ok(())
}

async fn wifi_test(value: &str) -> Result<()> {
    let on = match value {
        "on" => true,
        "off" => false,
        _ => anyhow::bail!("--wifi expects on|off"),
    };
    let client = nm_client::NmClient::connect().await?;
    println!("before: wireless={}", client.get_wireless_enabled().await?);
    client.set_wireless_enabled(on).await?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    println!("after: wireless={}", client.get_wireless_enabled().await?);
    Ok(())
}

async fn hotspot_test_run() -> Result<()> {
    use std::time::Duration;
    let client = nm_client::NmClient::connect().await?;
    let before = client.active_ssid().await.unwrap_or(None);
    println!("before: active={}", before.as_deref().unwrap_or("(none)"));

    let (ssid, psk) = client.create_hotspot().await?;
    println!("hotspot created: ssid={ssid} psk={psk}");
    for i in 0..10 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let m = client.refresh_model().await?;
        println!(
            "t+{}s active={} hs={} ip={}",
            (i + 1) * 3,
            m.active_ssid.as_deref().unwrap_or("(none)"),
            m.hotspot
                .as_ref()
                .map(|h| format!("{}(active={})", h.ssid, h.active))
                .unwrap_or_else(|| "-".into()),
            m.active_ipv4.as_deref().unwrap_or("-"),
        );
    }

    client.stop_hotspot().await?;
    println!("hotspot stopped, waiting for station reconnect…");
    for i in 0..6 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let active = client.active_ssid().await.unwrap_or(None);
        println!("t+{}s active={}", (i + 1) * 3, active.as_deref().unwrap_or("(none)"));
        if active.is_some() {
            break;
        }
    }

    if client.active_ssid().await.unwrap_or(None).is_none() {
        if let Some(prev) = before {
            println!("autoconnect did not restore; reactivating {prev}");
            client.connect_saved(&prev).await?;
            tokio::time::sleep(Duration::from_secs(5)).await;
            println!(
                "final active={}",
                client.active_ssid().await.unwrap_or(None).as_deref().unwrap_or("(none)")
            );
        }
    }
    Ok(())
}

async fn dump_aps() -> Result<()> {
    let client = Arc::new(nm_client::NmClient::connect().await?);
    info!(version = %client.version().await?, "connected to NetworkManager");
    let model = client.refresh_model().await?;
    println!("active: {}", model.active_ssid.as_deref().unwrap_or("(none)"));
    println!(
        "iface={} ipv4={} bitrate_kbps={} hotspot={}",
        model.active_iface.as_deref().unwrap_or("-"),
        model.active_ipv4.as_deref().unwrap_or("-"),
        model
            .active_bitrate_kbps
            .map(|b| b.to_string())
            .unwrap_or_else(|| "-".into()),
        model
            .hotspot
            .as_ref()
            .map(|h| format!("{}(active={})", h.ssid, h.active))
            .unwrap_or_else(|| "-".into()),
    );
    println!(
        "wifi_enabled={} networking_enabled={}",
        model.wifi_enabled, model.networking_enabled
    );
    println!("vpn_profiles={}", model.vpn_connections.len());
    for vpn in &model.vpn_connections {
        println!("vpn {} active={}", vpn.id, vpn.active);
    }
    for ap in &model.aps {
        println!(
            "{:>3}%  {:<32} {} {}",
            ap.strength,
            ap.ssid,
            if ap.secured { "🔒" } else { "  " },
            if ap.saved { "[saved]" } else { "" },
        );
    }
    audit_icons();
    Ok(())
}

fn audit_icons() {
    if gtk4::init().is_err() {
        println!("icons: no display, skipped");
        return;
    }
    let Some(display) = gtk4::gdk::Display::default() else {
        println!("icons: no display, skipped");
        return;
    };
    let theme = gtk4::IconTheme::for_display(&display);
    let theme_name = gtk4::Settings::default()
        .and_then(|s| s.gtk_icon_theme_name())
        .unwrap_or_default();
    println!("icon-theme: {theme_name}");
    for name in [
        "network-wireless-signal-none-symbolic",
        "network-wireless-signal-weak-symbolic",
        "network-wireless-signal-ok-symbolic",
        "network-wireless-signal-good-symbolic",
        "network-wireless-signal-excellent-symbolic",
        "network-wireless-disabled-symbolic",
        "network-wireless-acquiring-symbolic",
        "network-wireless-encrypted-symbolic",
        "network-wireless-symbolic",
        "network-wireless-hotspot-symbolic",
        "airplane-mode-symbolic",
        "network-vpn-symbolic",
        "pan-down-symbolic",
        "pan-up-symbolic",
        "scanner-symbolic",
        "camera-photo-symbolic",
        "list-add-symbolic",
        "preferences-system-symbolic",
        "go-previous-symbolic",
        "view-pin-symbolic",
        "system-shutdown-symbolic",
    ] {
        println!("  {:<45} {}", name, if theme.has_icon(name) { "OK" } else { "MISSING" });
    }
    println!("resolved files (size 24):");
    for name in [
        "network-wireless-signal-weak-symbolic",
        "network-wireless-signal-ok-symbolic",
        "network-wireless-signal-good-symbolic",
        "network-wireless-signal-excellent-symbolic",
    ] {
        let path = theme
            .lookup_icon(
                name,
                &[],
                24,
                1,
                gtk4::TextDirection::Ltr,
                gtk4::IconLookupFlags::empty(),
            )
            .file()
            .and_then(|f| f.path())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "-".into());
        println!("  {name:<45} {path}");
    }
}

fn take_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].clone())
}

fn wait_for_compositor(timeout: std::time::Duration) -> Result<()> {
    if std::env::var("WAYLAND_DISPLAY").is_err() && std::env::var("DISPLAY").is_err() {
        anyhow::bail!("cannot start: no Wayland or X11 compositor is reachable");
    }
    gtk4::init()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(display) = gtk4::gdk::Display::default() {
            let theme_ready = gtk4::Settings::default()
                .and_then(|s| s.gtk_icon_theme_name())
                .is_some()
                && (gtk4::IconTheme::for_display(&display)
                    .has_icon("network-wireless-signal-ok-symbolic")
                    || gtk4::IconTheme::for_display(&display)
                        .has_icon("network-wireless-symbolic"));
            if theme_ready {
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out after {}s waiting for the desktop theme and icon theme to become \
                 available (the compositor may still be starting up)",
                timeout.as_secs()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

async fn backend_main(
    model_feed: async_channel::Sender<state::UiEvent>,
    toggle_tx: async_channel::Sender<Option<(i32, i32)>>,
    quit_tx: async_channel::Sender<()>,
    tray_ev_tx: async_channel::Sender<tray::TrayEvent>,
    tray_ev_rx: async_channel::Receiver<tray::TrayEvent>,
) {

    let client = loop {
        match nm_client::NmClient::connect().await {
            Ok(c) => break Arc::new(c),
            Err(e) => {
                tracing::warn!("NM connect failed, retrying in 5s: {e:#}");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    };

    let (watch_tx, watch_rx) = state::channel();
    let _model_loop = nm_client::signals::spawn_model_loop(client.clone(), watch_tx.clone());
    let _signal_watch = nm_client::signals::spawn_signal_watcher(client.clone(), watch_tx.clone());

    let initial = client.refresh_model().await.unwrap_or_default();
    let _ = watch_tx.send(initial.clone());
    let _ = model_feed.try_send(state::UiEvent::Model(initial.clone()));

    let secrets = secret_agent::SecretStore::new();

    let hotspot_expected = Arc::new(AtomicBool::new(false));
    if let Err(e) = secret_agent::register(&client, secrets.clone(), model_feed.clone()).await {
        tracing::warn!("SecretAgent registration failed (inline passwords unavailable): {e:#}");

        let client = client.clone();
        let secrets = secrets.clone();
        let model_feed = model_feed.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            if let Err(e) = secret_agent::register(&client, secrets, model_feed).await {
                tracing::warn!("SecretAgent registration retry failed: {e:#}");
            }
        });
    }

    let _heartbeat = tokio::spawn({
        let client = client.clone();
        let watch_tx = watch_tx.clone();
        let watch_rx = watch_rx.clone();
        let model_feed = model_feed.clone();
        let secrets = secrets.clone();
        async move {
            let mut tick =
                tokio::time::interval(std::time::Duration::from_secs(15));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut offline = false;
            loop {
                tick.tick().await;
                if client.nm.version().await.is_ok() {
                    if offline {
                        offline = false;
                        info!("NetworkManager back, refreshing");
                        if let Err(e) = secret_agent::register(
                            &client,
                            secrets.clone(),
                            model_feed.clone(),
                        )
                        .await
                        {
                            tracing::warn!("SecretAgent re-registration failed: {e:#}");
                        }
                        match client.refresh_model().await {
                            Ok(m) => {
                                let _ = watch_tx.send(m);
                            }
                            Err(e) => tracing::warn!("refresh after NM recovery failed: {e:#}"),
                        }
                    }
                    continue;
                }
                if !offline {
                    offline = true;
                    tracing::warn!("NetworkManager unreachable, showing last-known state");
                    let mut m = watch_rx.borrow().clone();
                    m.nm_online = false;
                    let _ = watch_tx.send(m);
                }
            }
        }
    });

    let _speeds = tokio::spawn({
        let watch_rx = watch_rx.clone();
        let model_feed = model_feed.clone();
        async fn counters(iface: &str) -> Option<(u64, u64)> {
            let rx = tokio::fs::read_to_string(format!(
                "/sys/class/net/{iface}/statistics/rx_bytes"
            ))
            .await
            .ok()?;
            let tx = tokio::fs::read_to_string(format!(
                "/sys/class/net/{iface}/statistics/tx_bytes"
            ))
            .await
            .ok()?;
            Some((
                rx.trim().parse().ok()?,
                tx.trim().parse().ok()?,
            ))
        }
        async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last: Option<(String, u64, u64, std::time::Instant)> = None;
            let mut sent_none = false;
            loop {
                tick.tick().await;
                let iface = watch_rx.borrow().active_iface.clone();
                let Some(iface) = iface else {
                    last = None;
                    if !sent_none {
                        sent_none = true;
                        let _ = model_feed
                            .try_send(state::UiEvent::Speeds {
                                up_bps: None,
                                down_bps: None,
                            });
                    }
                    continue;
                };
                sent_none = false;
                let now = std::time::Instant::now();
                let Some((rx, tx)) = counters(&iface).await else {
                    continue;
                };
                if let Some((prev_iface, prev_rx, prev_tx, prev_t)) = last.replace((iface.clone(), rx, tx, now)) {
                    if prev_iface == iface && rx >= prev_rx && tx >= prev_tx {
                        let dt = now.duration_since(prev_t).as_secs_f64().clamp(0.5, 10.0);
                        let down = (rx - prev_rx) as f64 / dt;
                        let up = (tx - prev_tx) as f64 / dt;
                        let _ = model_feed.try_send(state::UiEvent::Speeds {
                            up_bps: Some(up as u64),
                            down_bps: Some(down as u64),
                        });
                    }
                }
            }
        }
    });

    let tray_deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let tray_handle = loop {
        if let Ok(h) = tray::spawn(initial.clone(), tray_ev_tx.clone()).await {
            info!("tray registered as org.kde.StatusNotifierItem (id=rnetapplet)");
            break h;
        }
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        if let Ok(h) = tray::spawn(initial.clone(), tray_ev_tx.clone()).await {
            info!("tray registered as org.kde.StatusNotifierItem (id=rnetapplet)");
            break h;
        }
        if std::time::Instant::now() <= tray_deadline {
            tracing::warn!("SNI tray host not ready, retrying");
        } else {
            tracing::error!("tray spawn failed after multiple attempts");
            return;
        }
    };
    let _tray_updater = tray::spawn_updater(tray_handle.clone(), watch_rx.clone());

    let feed_task = tokio::spawn({
        let mut rx = watch_rx.clone();
        let model_feed = model_feed.clone();
        async move {
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                let m = rx.borrow().clone();
                if model_feed.send(state::UiEvent::Model(m)).await.is_err() {
                    break;
                }
            }
        }
    });

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("ctrl-c, shutting down");
                break;
            }
            ev = tray_ev_rx.recv() => {
                match ev {
                    Ok(tray::TrayEvent::TogglePopup(pos)) => {
                        tracing::debug!("backend got TogglePopup");
                        let _ = toggle_tx.try_send(pos);
                    }
                    Ok(tray::TrayEvent::SetWifi(on)) => {
                        info!(on, "Wi-Fi toggle requested");
                        if let Err(e) = client.set_wireless_enabled(on).await {
                            tracing::warn!("set_wireless_enabled failed: {e:#}");
                        }

                        match client.refresh_model().await {
                            Ok(m) => { let _ = watch_tx.send(m); }
                            Err(e) => tracing::warn!("refresh after wifi toggle failed: {e:#}"),
                        }
                        nm_client::connect::refresh_staggered(client.clone(), watch_tx.clone());
                    }
                    Ok(state::BackendCmd::ConnectOpen(ssid)) => {
                        info!(ssid, "connect requested (open)");
                        let prev = client.active_wifi_snapshot().await.map(|(p, s)| {
                            nm_client::connect::PrevWifi { profile: p, ssid: s }
                        });
                        match client.connect_open(&ssid).await {
                            Ok(()) => {
                                nm_client::connect::spawn_restore_guard(
                                    client.clone(),
                                    watch_rx.clone(),
                                    model_feed.clone(),
                                    ssid.clone(),
                                    prev,
                                    std::time::Duration::from_secs(30),
                                );
                            }
                            Err(e) => {
                                tracing::warn!("connect_open {ssid} failed: {e:#}");
                                let _ = client.restore_previous(prev).await;
                                let _ = model_feed.try_send(state::UiEvent::SsidError {
                                    ssid: ssid.clone(),
                                    message: format!("Could not connect: {e:#}"),
                                });
                            }
                        }
                        nm_client::connect::refresh_staggered(client.clone(), watch_tx.clone());
                    }
                    Ok(state::BackendCmd::ConnectSaved(ssid)) => {
                        info!(ssid, "connect requested (saved profile)");
                        let prev = client.active_wifi_snapshot().await.map(|(p, s)| {
                            nm_client::connect::PrevWifi { profile: p, ssid: s }
                        });
                        match client.connect_saved(&ssid).await {
                            Ok(()) => {
                                nm_client::connect::spawn_restore_guard(
                                    client.clone(),
                                    watch_rx.clone(),
                                    model_feed.clone(),
                                    ssid.clone(),
                                    prev,
                                    std::time::Duration::from_secs(30),
                                );
                            }
                            Err(e) => {
                                tracing::warn!("connect_saved {ssid} failed: {e:#}");
                                let _ = client.restore_previous(prev).await;
                                let _ = model_feed.try_send(state::UiEvent::SsidError {
                                    ssid: ssid.clone(),
                                    message: format!("Could not connect: {e:#}"),
                                });
                            }
                        }
                        nm_client::connect::refresh_staggered(client.clone(), watch_tx.clone());
                    }
                    Ok(state::BackendCmd::DisconnectActive) => {
                        info!("disconnect requested");
                        if let Err(e) = client.disconnect_active().await {
                            tracing::warn!("disconnect failed: {e:#}");
                        }
                        nm_client::connect::refresh_staggered(client.clone(), watch_tx.clone());
                    }
                    Ok(state::BackendCmd::ConnectSecure { ssid, psk }) => {
                        info!(ssid, "connect requested (inline password)");
                        let prev = client.active_wifi_snapshot().await.map(|(p, s)| {
                            nm_client::connect::PrevWifi { profile: p, ssid: s }
                        });
                        match client.connect_secure(&ssid, &psk).await {
                            Ok(()) => {
                                nm_client::connect::spawn_restore_guard(
                                    client.clone(),
                                    watch_rx.clone(),
                                    model_feed.clone(),
                                    ssid.clone(),
                                    prev,
                                    std::time::Duration::from_secs(30),
                                );
                            }
                            Err(e) => {
                                tracing::warn!("connect_secure {ssid} failed: {e:#}");
                                let _ = client.restore_previous(prev).await;
                                let _ = model_feed.try_send(state::UiEvent::SsidError {
                                    ssid: ssid.clone(),
                                    message: format!("Could not connect: {e:#}"),
                                });
                            }
                        }
                        nm_client::connect::refresh_staggered(client.clone(), watch_tx.clone());
                    }
                    Ok(state::BackendCmd::ProvideSecret { path, psk }) => {
                        info!(%path, "retry password provided");
                        if let Err(e) = nm_client::connect::validate_psk(&psk) {
                            tracing::warn!("rejected invalid retry password for {path}: {e:#}");
                        } else {
                            secrets.provide(&path, psk).await;
                        }
                    }
                    Ok(state::BackendCmd::SetAirplane(on)) => {
                        info!(on, "airplane mode requested");
                        if let Err(e) = client.set_airplane_mode(on).await {
                            tracing::warn!("set_airplane_mode failed: {e:#}");
                        }
                        match client.refresh_model().await {
                            Ok(m) => { let _ = watch_tx.send(m); }
                            Err(e) => tracing::warn!("refresh after airplane toggle failed: {e:#}"),
                        }
                        nm_client::connect::refresh_staggered(client.clone(), watch_tx.clone());
                    }
                    Ok(state::BackendCmd::CreateHotspot { ssid, psk }) => {
                        info!(ssid, "hotspot requested");
                        match client.create_hotspot_with(&ssid, &psk).await {
                            Ok((ssid, psk)) => {
                                hotspot_expected.store(true, Ordering::Relaxed);

                                tokio::spawn({
                                    let client = client.clone();
                                    let watch_tx = watch_tx.clone();
                                    let model_feed = model_feed.clone();
                                    let hotspot_expected = hotspot_expected.clone();
                                    async move {
                                        tokio::time::sleep(std::time::Duration::from_secs(25)).await;
                                        if !hotspot_expected.load(Ordering::Relaxed) {
                                            return;
                                        }
                                        let active = client
                                            .refresh_model()
                                            .await
                                            .map(|m| {
                                                m.hotspot.as_ref().map(|h| h.active).unwrap_or(false)
                                            })
                                            .unwrap_or(false);
                                        if active {
                                            return;
                                        }
                                        tracing::warn!("hotspot never activated; stopping it");
                                        let _ = client.stop_hotspot().await;
                                        hotspot_expected.store(false, Ordering::Relaxed);
                                        let _ = model_feed.try_send(state::UiEvent::BackendError(
                                            "Hotspot failed to start (this Wi-Fi driver did not bring up AP mode)".into(),
                                        ));
                                        if let Ok(m) = client.refresh_model().await {
                                            let _ = watch_tx.send(m);
                                        }
                                    }
                                });

                                match client.refresh_model().await {
                                    Ok(mut m) => {
                                        if let Some(hs) = m.hotspot.as_mut()
                                            && hs.ssid == ssid {
                                                hs.psk = Some(psk);
                                            }
                                        let _ = watch_tx.send(m);
                                    }
                                    Err(e) => tracing::warn!("refresh after hotspot failed: {e:#}"),
                                }
                            }
                            Err(e) => {
                                tracing::warn!("create_hotspot failed: {e:#}");
                                let _ = model_feed.try_send(state::UiEvent::BackendError(
                                    format!("Could not start hotspot: {e:#}"),
                                ));
                            }
                        }
                        nm_client::connect::refresh_staggered(client.clone(), watch_tx.clone());
                    }
                    Ok(state::BackendCmd::StopHotspot) => {
                        info!("hotspot stop requested");
                        hotspot_expected.store(false, Ordering::Relaxed);
                        if let Err(e) = client.stop_hotspot().await {
                            tracing::warn!("stop_hotspot failed: {e:#}");
                            let _ = model_feed.try_send(state::UiEvent::BackendError(
                                format!("Could not stop hotspot: {e:#}"),
                            ));
                        }
                        nm_client::connect::refresh_staggered(client.clone(), watch_tx.clone());
                    }
                    Ok(state::BackendCmd::Forget(ssid)) => {
                        info!(ssid, "forget saved network requested");
                        if let Err(e) = client.forget_saved(&ssid).await {
                            tracing::warn!("forget {ssid} failed: {e:#}");
                            let _ = model_feed.try_send(state::UiEvent::SsidError {
                                ssid: ssid.clone(),
                                message: format!("Could not forget: {e:#}"),
                            });
                        }
                        match client.refresh_model().await {
                            Ok(m) => { let _ = watch_tx.send(m); }
                            Err(e) => tracing::warn!("refresh after forget failed: {e:#}"),
                        }
                        nm_client::connect::refresh_staggered(client.clone(), watch_tx.clone());
                    }
                    Ok(state::BackendCmd::ConnectHidden { ssid, psk }) => {
                        info!(ssid, "hidden network connect requested");
                        if let Err(e) = client.connect_hidden(&ssid, &psk).await {
                            tracing::warn!("connect_hidden {ssid} failed: {e:#}");
                            let _ = model_feed.try_send(state::UiEvent::SsidError {
                                ssid: ssid.clone(),
                                message: format!("Could not connect: {e:#}"),
                            });
                        }
                        nm_client::connect::refresh_staggered(client.clone(), watch_tx.clone());
                    }
                    Ok(state::BackendCmd::RefreshTray) => {
                        info!("theme change, re-emitting tray icon");
                        match client.refresh_model().await {
                            Ok(m) => { let _ = watch_tx.send(m); }
                            Err(e) => tracing::warn!("tray refresh failed: {e:#}"),
                        }
                    }
                    Ok(tray::TrayEvent::Quit) | Err(_) => break,
                }
            }
        }
    }

    feed_task.abort();
    tray_handle.shutdown().await;
    let _ = quit_tx.try_send(());
}
