mod auto;
mod nm_client;
mod qr;
mod qr_scan;
mod secret_agent;
mod state;
mod tray;
mod ui;
use anyhow::Result;
use gtk4::gio::prelude::FileExt as _;
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const HELP: &str = "rnetapplet — NetworkManager tray applet for wlroots compositors
USAGE:
    rnetapplet [FLAGS] [OPTIONS]
FLAGS:
    --dump-aps            Print visible access points and exit
    --camera-test         Probe the camera + QR decode path and exit
    --scan-qr             Run the QR scanner window and print the result as JSON
    --popup-child         Run the network popup window (spawned by the tray process)
    --hotspot-test        Create a hotspot, observe, stop it, restore Wi-Fi, exit
    --no-auto-failover    Do not nudge a saved network when connected but offline
    -h, --help            Print this help
    -V, --version         Print version
OPTIONS:
    --connect-saved <SSID>  Activate a saved profile and exit
    --psk <SECRET>          With --connect-saved: stage the password for the attempt; stored only after the handshake succeeds
    --wifi <on|off>         Set the Wi-Fi radio and exit
ENV:
    RUST_LOG                Tracing filter (default: info), e.g. RUST_LOG=rnetapplet=debug
    RNETAPPLET_NO_AUTO_FAILOVER=1  Same as --no-auto-failover
";
fn process_is_rnetapplet(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|comm| comm.trim() == "rnetapplet")
        .unwrap_or(false)
}

fn claim_instance_lock() -> Result<()> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let lock = dir.join("rnetapplet.lock");
    for _ in 0..2 {
        if std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock)
            .is_ok()
        {
            std::fs::write(&lock, std::process::id().to_string())?;
            return Ok(());
        }
        let live = std::fs::read_to_string(&lock)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
            .filter(|pid| *pid != std::process::id())
            .is_some_and(process_is_rnetapplet);
        if live {
            let pid = std::fs::read_to_string(&lock)
                .map(|text| text.trim().to_string())
                .unwrap_or_else(|_| "?".into());
            anyhow::bail!(
                "another rnetapplet instance is already running (pid {pid}); refusing a second copy"
            );
        }
        let _ = std::fs::remove_file(&lock);
    }
    anyhow::bail!("could not claim the rnetapplet instance lock")
}
async fn shutdown_signal() {
    let mut term = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("cannot listen for SIGTERM: {e}");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

fn main() -> Result<()> {
    if std::env::var_os("GSK_RENDERER").is_none() {
        unsafe { std::env::set_var("GSK_RENDERER", "cairo") };
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rnetapplet=info".into()),
        )
        .with_ansi(false)
        .init();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        let backtrace = std::backtrace::Backtrace::force_capture();
        let msg = format!(
            "panic at {}\npayload: {}\nthread: {}\nbacktrace:\n{backtrace}\n",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            info,
            std::thread::current().name().unwrap_or("<unnamed>"),
        );
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state"))
            });
        if let Some(base) = base {
            let dir = base.join("rnetapplet");
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("crash.log"))
            {
                use std::io::Write as _;
                let _ = f.write_all(msg.as_bytes());
            }
        }
        eprintln!("{msg}");
    }));
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("rnetapplet {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    validate_args(&args)?;
    let dump = args.iter().any(|a| a == "--dump-aps");
    let connect_saved = take_value(&args, "--connect-saved");
    let psk = take_value(&args, "--psk");
    let set_wifi = take_value(&args, "--wifi");
    let camera_test = args.iter().any(|a| a == "--camera-test");
    let scan_qr = args.iter().any(|a| a == "--scan-qr");
    let hotspot_test = args.iter().any(|a| a == "--hotspot-test");
    if let Some(v) = &set_wifi
        && v != "on"
        && v != "off"
    {
        anyhow::bail!("--wifi expects on|off, got {v:?}");
    }
    if dump
        || connect_saved.is_some()
        || set_wifi.is_some()
        || camera_test
        || hotspot_test
        || scan_qr
        || args.iter().any(|a| a == "--popup-child")
    {
        if camera_test {
            return qr_scan::camera_self_test();
        }
        if scan_qr {
            let parent = take_value(&args, "--parent-pid").and_then(|s| s.parse::<u32>().ok());
            return qr_scan::scanner_child(parent);
        }
        if args.iter().any(|a| a == "--popup-child") {
            let x = take_value(&args, "--popup-x").and_then(|s| s.parse::<i32>().ok());
            let y = take_value(&args, "--popup-y").and_then(|s| s.parse::<i32>().ok());
            let pos = x.zip(y);
            return ui::child::run_popup_child(pos);
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        if hotspot_test {
            return rt.block_on(hotspot_test_run());
        }
        if let Some(ssid) = connect_saved {
            return rt.block_on(connect_saved_test(&ssid, psk.as_deref()));
        }
        if let Some(v) = set_wifi {
            return rt.block_on(wifi_test(&v));
        }
        return rt.block_on(dump_aps());
    }
    claim_instance_lock()?;
    if std::env::var_os("GDK_BACKEND").is_none() {
        unsafe { std::env::set_var("GDK_BACKEND", "wayland,x11") };
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .max_blocking_threads(4)
        .build()?;
    let (client, initial) = rt.block_on(async {
        let client = Arc::new(nm_client::NmClient::connect().await?);
        info!(version = %client.version().await?, "connected to NetworkManager");
        let initial = client.refresh_model().await.unwrap_or_else(|e| {
            tracing::warn!("initial model refresh failed: {e:#}");
            state::Model::empty()
        });
        anyhow::Ok((client, initial))
    })?;
    let (model_feed_tx, model_feed_rx) = async_channel::bounded::<state::UiEvent>(64);
    let (toggle_tx, toggle_rx) = async_channel::bounded::<Option<(i32, i32)>>(4);
    let popup_visible = client.popup_visible();
    let scan_frozen = Arc::new(AtomicBool::new(false));
    let (tray_ev_tx, tray_ev_rx) = async_channel::bounded::<tray::TrayEvent>(64);
    rt.block_on(backend_main(
        client,
        initial.clone(),
        model_feed_tx,
        model_feed_rx,
        toggle_tx,
        toggle_rx,
        tray_ev_tx.clone(),
        tray_ev_rx,
        popup_visible.clone(),
        scan_frozen.clone(),
        !args.iter().any(|a| a == "--no-auto-failover")
            && std::env::var_os("RNETAPPLET_NO_AUTO_FAILOVER") != Some("1".into()),
    ));
    Ok(())
}
fn validate_args(args: &[String]) -> Result<()> {
    const FLAGS: [&str; 13] = [
        "--dump-aps",
        "--connect-saved",
        "--wifi",
        "--psk",
        "--camera-test",
        "--scan-qr",
        "--parent-pid",
        "--popup-child",
        "--popup-x",
        "--popup-y",
        "--hotspot-test",
        "--no-auto-failover",
        "--version",
    ];
    const VALUED: [&str; 6] = [
        "--connect-saved",
        "--wifi",
        "--psk",
        "--parent-pid",
        "--popup-x",
        "--popup-y",
    ];
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if FLAGS.contains(&a) {
            if VALUED.contains(&a) {
                if i + 1 >= args.len() {
                    anyhow::bail!("{a} needs a value; try `{a} --help`");
                }
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        anyhow::bail!("unknown option {a:?}; run `rnetapplet --help` for the list");
    }
    Ok(())
}
async fn connect_saved_test(ssid: &str, psk: Option<&str>) -> Result<()> {
    let client = nm_client::NmClient::connect().await?;
    info!(version = %client.version().await?, "connected to NetworkManager");
    let secrets = secret_agent::SecretStore::new();
    let prev = client
        .active_wifi_snapshot()
        .await
        .map(|(profile, ssid)| nm_client::connect::PrevWifi { profile, ssid });
    let mut commit = None;
    match psk {
        Some(secret) => {
            let retry = client
                .retry_saved_with_psk(ssid, state::Secret::from(secret), &secrets)
                .await?;
            commit = retry.commit;
            println!("supplied password staged, not written; waiting for the handshake…");
        }
        None => {
            client.connect_saved(ssid).await?;
        }
    }
    let mut last = None;
    for i in 1..=10 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let active = client.active_ssid().await.unwrap_or(None);
        println!(
            "t+{}s active={}",
            i * 2,
            active.as_deref().unwrap_or("(none)")
        );
        if active.as_deref() == Some(ssid) {
            if let Some(path) = &commit {
                if let Some(proof) = secrets.take_served_psk(&path.to_string()).await {
                    client.update_psk(path, proof.as_str()).await?;
                    println!("RESULT: connected; the verified password was saved");
                } else {
                    println!(
                        "RESULT: connected with the stored password; the supplied password was not needed and was not saved"
                    );
                }
            } else {
                println!("RESULT: connected to {ssid}");
            }
            return Ok(());
        }
        last = active;
    }
    println!("RESULT: failed; the stored password was never touched");
    if let Some(prev) = prev {
        let _ = client.reactivate_previous(&prev.ssid, &prev.profile).await;
        println!("RESULT: restored {}", prev.ssid);
    }
    println!(
        "RESULT: failed (active={})",
        last.as_deref().unwrap_or("(none)")
    );
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
        println!(
            "t+{}s active={}",
            (i + 1) * 3,
            active.as_deref().unwrap_or("(none)")
        );
        if active.is_some() {
            break;
        }
    }
    if client.active_ssid().await.unwrap_or(None).is_none()
        && let Some(prev) = before
    {
        println!("autoconnect did not restore; reactivating {prev}");
        client.connect_saved(&prev).await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        println!(
            "final active={}",
            client
                .active_ssid()
                .await
                .unwrap_or(None)
                .as_deref()
                .unwrap_or("(none)")
        );
    }
    Ok(())
}
async fn dump_aps() -> Result<()> {
    let client = Arc::new(nm_client::NmClient::connect().await?);
    info!(version = %client.version().await?, "connected to NetworkManager");
    if let Ok(Some(path)) = client.wifi_device_path().await {
        let t0 = std::time::Instant::now();
        match client.scan_and_wait(&path, nm_client::SCAN_WAIT).await {
            Ok(true) => info!(secs = ?t0.elapsed().as_secs_f32(), "scan completed"),
            Ok(false) => tracing::warn!("scan window elapsed"),
            Err(e) => tracing::warn!("scan failed: {e:#}"),
        }
    }
    let model = client.refresh_model().await?;
    println!(
        "active: {}",
        model.active_ssid.as_deref().unwrap_or("(none)")
    );
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
    for vpn in model.vpn_connections.iter() {
        println!("vpn {} active={}", vpn.id, vpn.active);
    }
    for ap in model.aps.iter() {
        println!(
            "{:>3}%  {:<32} {} {}",
            ap.strength,
            ap.ssid,
            if ap.security.is_secured() {
                "🔒"
            } else {
                "  "
            },
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
        "network-wireless-offline-symbolic",
        "network-wireless-disabled-symbolic",
        "network-wireless-no-route-symbolic",
        "network-wired-no-route-symbolic",
        "network-wired-symbolic",
        "network-offline-symbolic",
        "network-error-symbolic",
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
        println!(
            "  {:<45} {}",
            name,
            if theme.has_icon(name) {
                "OK"
            } else {
                "MISSING"
            }
        );
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
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}
#[allow(clippy::too_many_arguments)]
async fn backend_main(
    client: Arc<nm_client::NmClient>,
    initial: state::Model,
    model_feed: async_channel::Sender<state::UiEvent>,
    model_feed_rx: async_channel::Receiver<state::UiEvent>,
    toggle_tx: async_channel::Sender<Option<(i32, i32)>>,
    toggle_rx: async_channel::Receiver<Option<(i32, i32)>>,
    tray_ev_tx: async_channel::Sender<tray::TrayEvent>,
    tray_ev_rx: async_channel::Receiver<tray::TrayEvent>,
    popup_visible: Arc<AtomicBool>,
    scan_frozen: Arc<AtomicBool>,
    auto_failover: bool,
) {
    let (watch_tx, watch_rx) = state::channel();
    let model: Arc<state::Model> = Arc::new(initial);
    let _ = watch_tx.send(model.clone());
    let _ = model_feed.try_send(state::UiEvent::Model(model.clone()));
    let refresher = Arc::new(nm_client::signals::Refresher::spawn(
        client.clone(),
        watch_tx.clone(),
        popup_visible.clone(),
        scan_frozen.clone(),
    ));
    let signal_watch =
        nm_client::signals::spawn_signal_watcher(client.clone(), Arc::downgrade(&refresher));
    let secrets = secret_agent::SecretStore::new();
    let hotspot_expected = Arc::new(AtomicBool::new(false));
    if let Err(e) = secret_agent::register(&client, secrets.clone(), model_feed.clone()).await {
        tracing::warn!("SecretAgent registration failed (inline passwords unavailable): {e:#}");
        for attempt in 1..=2 {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            match secret_agent::register(&client, secrets.clone(), model_feed.clone()).await {
                Ok(()) => {
                    tracing::info!("SecretAgent registered on retry");
                    break;
                }
                Err(e) if attempt == 2 => {
                    tracing::warn!("SecretAgent registration retry failed: {e:#}");
                }
                Err(_) => {}
            }
        }
    }
    let _heartbeat = tokio::spawn({
        let client = client.clone();
        let model_feed = model_feed.clone();
        let secrets = secrets.clone();
        let refresher = refresher.clone();
        async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(15));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut offline = false;
            let mut ticks = 0u64;
            loop {
                tick.tick().await;
                ticks += 1;
                if ticks.is_multiple_of(4) {
                    let tasks = tokio::runtime::Handle::current()
                        .metrics()
                        .num_alive_tasks() as u64;
                    info!(tasks, "backend heartbeat");
                }
                if ticks.is_multiple_of(20) {
                    crate::ui::placement::trim_memory();
                }
                if client.nm.version().await.is_ok() {
                    if offline {
                        offline = false;
                        info!("NetworkManager back, refreshing");
                        if let Err(e) =
                            secret_agent::register(&client, secrets.clone(), model_feed.clone())
                                .await
                        {
                            tracing::warn!("SecretAgent re-registration failed: {e:#}");
                        }
                        refresher.request(nm_client::signals::Cmd::Scan);
                    }
                    refresher.request(nm_client::signals::Cmd::Refresh);
                    continue;
                }
                if !offline {
                    offline = true;
                    tracing::warn!("NetworkManager unreachable, showing last-known state");
                    refresher.publish_with(|m| m.nm_online = false);
                }
            }
        }
    });
    let _speeds = tokio::spawn({
        let watch_rx = watch_rx.clone();
        let model_feed = model_feed.clone();
        let popup_visible = popup_visible.clone();
        async fn counters(iface: &str) -> Option<(u64, u64)> {
            let rx = std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/rx_bytes"))
                .ok()?;
            let tx = std::fs::read_to_string(format!("/sys/class/net/{iface}/statistics/tx_bytes"))
                .ok()?;
            Some((rx.trim().parse().ok()?, tx.trim().parse().ok()?))
        }
        async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last: Option<(String, u64, u64, std::time::Instant)> = None;
            let mut sent_none = false;
            loop {
                tick.tick().await;
                if !popup_visible.load(Ordering::Relaxed) {
                    last = None;
                    continue;
                }
                let iface = watch_rx.borrow().active_iface.clone();
                let Some(iface) = iface else {
                    last = None;
                    if !sent_none {
                        sent_none = true;
                        let _ = model_feed.try_send(state::UiEvent::Speeds {
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
                if let Some((prev_iface, prev_rx, prev_tx, prev_t)) =
                    last.replace((iface.clone(), rx, tx, now))
                    && prev_iface == iface
                    && rx >= prev_rx
                    && tx >= prev_tx
                {
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
    });
    let tray_service = tray::TrayService::start(
        watch_rx.borrow().clone(),
        tray_ev_tx.clone(),
        toggle_tx.clone(),
    );
    let last_manual_disconnect: Arc<tokio::sync::Mutex<Option<std::time::Instant>>> =
        Arc::new(tokio::sync::Mutex::new(None));
    if auto_failover {
        auto::spawn(
            watch_rx.clone(),
            tray_ev_tx.clone(),
            last_manual_disconnect.clone(),
        );
    }
    let tray_sender = tray_service.clone_sender();
    let tray_bridge = {
        let watch_rx = watch_rx.clone();
        tokio::spawn(async move {
            let mut rx = watch_rx;
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                let m = rx.borrow().clone();
                match tray_sender.try_send(m) {
                    Ok(()) => {}
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                    Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                }
            }
        })
    };
    let feed_task = tokio::spawn({
        let mut rx = watch_rx.clone();
        let model_feed = model_feed.clone();
        async move {
            let mut dropped = 0u64;
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                let m = rx.borrow().clone();
                let n = m.aps.len();
                match model_feed.try_send(state::UiEvent::Model(m)) {
                    Ok(()) => {
                        if dropped > 0 {
                            tracing::info!("ui caught up after {dropped} dropped models");
                            dropped = 0;
                        }
                    }
                    Err(async_channel::TrySendError::Full(_)) => {
                        dropped += 1;
                        if dropped == 1 || dropped.is_multiple_of(50) {
                            tracing::warn!(
                                "ui not consuming models ({dropped} dropped); latest state will follow"
                            );
                        }
                    }
                    Err(async_channel::TrySendError::Closed(_)) => break,
                }
                tracing::debug!(aps = n, "model handed to ui");
            }
        }
    });
    struct PopupProc {
        child: tokio::process::Child,
        stdin: tokio::process::ChildStdin,
    }
    async fn write_event(
        stdin: &mut tokio::process::ChildStdin,
        ev: &state::UiEvent,
    ) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt as _;
        let mut line = serde_json::to_vec(ev)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push(b'\n');
        stdin.write_all(&line).await?;
        stdin.flush().await
    }
    async fn spawn_popup(
        watch_rx: &state::ModelRx,
        tray_ev_tx: &async_channel::Sender<tray::TrayEvent>,
        visible: &Arc<AtomicBool>,
        pos: Option<(i32, i32)>,
    ) -> Option<PopupProc> {
        let exe = std::env::current_exe().ok()?;
        let mut child = tokio::process::Command::new(exe);
        child.arg("--popup-child");
        if let Some((x, y)) = pos {
            child
                .arg("--popup-x")
                .arg(x.to_string())
                .arg("--popup-y")
                .arg(y.to_string());
        }
        let mut child = child
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let mut stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let tray_ev_tx = tray_ev_tx.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt as _;
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(cmd) = serde_json::from_str::<state::BackendCmd>(&line)
                    && tray_ev_tx.send(cmd).await.is_err()
                {
                    break;
                }
            }
        });
        let initial = state::UiEvent::Model(watch_rx.borrow().clone());
        if write_event(&mut stdin, &initial).await.is_err() {
            return None;
        }
        visible.store(true, Ordering::Relaxed);
        Some(PopupProc { child, stdin })
    }
    let popup_manager = {
        let watch_rx = watch_rx.clone();
        let tray_ev_tx = tray_ev_tx.clone();
        let popup_visible = popup_visible.clone();
        tokio::spawn(async move {
            let mut proc: Option<PopupProc> = None;
            loop {
                if let Some(p) = proc.as_mut() {
                    match p.child.try_wait() {
                        Ok(Some(_)) | Err(_) => {
                            proc = None;
                            popup_visible.store(false, Ordering::Relaxed);
                        }
                        Ok(None) => {}
                    }
                }
                tokio::select! {
                    r = toggle_rx.recv() => {
                        let Ok(pos) = r else { break };
                        match proc.take() {
                            Some(mut p) => {
                                let _ = p.child.start_kill();
                                let _ = p.child.wait().await;
                                popup_visible.store(false, Ordering::Relaxed);
                            }
                            None => {
                                proc = spawn_popup(&watch_rx, &tray_ev_tx, &popup_visible, pos).await;
                            }
                        }
                    }
                    r = model_feed_rx.recv() => {
                        let Ok(ev) = r else { break };
                        if let Some(p) = proc.as_mut()
                            && write_event(&mut p.stdin, &ev).await.is_err()
                        {
                            proc = None;
                            popup_visible.store(false, Ordering::Relaxed);
                        }
                    }
                }
            }
        })
    };
    let ctx = ConnectCtx {
        client: client.clone(),
        watch_rx: watch_rx.clone(),
        model_feed: model_feed.clone(),
        refresher: refresher.clone(),
        secrets: secrets.clone(),
        scan_frozen: scan_frozen.clone(),
    };
    macro_rules! settle {
        () => {
            refresher.request(nm_client::signals::Cmd::Refresh)
        };
    }
    async fn scan_once_now(client: &Arc<nm_client::NmClient>) -> bool {
        let path = client.wifi_device_path().await.ok().flatten();
        let Some(path) = path else {
            return false;
        };
        client
            .scan_and_wait(&path, nm_client::SCAN_WAIT)
            .await
            .unwrap_or(false)
    }
    async fn with_bus_timeout<T>(
        secs: u64,
        what: &str,
        fut: impl std::future::Future<Output = T>,
    ) -> Option<T> {
        match tokio::time::timeout(std::time::Duration::from_secs(secs), fut).await {
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!("{what} timed out after {secs}s; carrying on");
                None
            }
        }
    }
    #[derive(Clone)]
    struct ConnectCtx {
        client: Arc<nm_client::NmClient>,
        watch_rx: state::ModelRx,
        model_feed: async_channel::Sender<state::UiEvent>,
        refresher: Arc<nm_client::signals::Refresher>,
        secrets: Arc<secret_agent::SecretStore>,
        scan_frozen: Arc<AtomicBool>,
    }
    enum ConnectKind {
        Open,
        Saved,
        Secure(state::Secret),
        RetrySaved(state::Secret),
        Hidden(state::Secret),
    }
    async fn guarded_connect(ctx: ConnectCtx, ssid: state::Ssid, kind: ConnectKind) {
        tracing::info!(ssid = %ssid.as_str(), "connect started");
        ctx.scan_frozen.store(true, Ordering::Relaxed);
        let prev = with_bus_timeout(8, "previous-connection snapshot", async {
            ctx.client
                .active_wifi_snapshot()
                .await
                .map(|(p, s)| nm_client::connect::PrevWifi {
                    profile: p,
                    ssid: s,
                })
        })
        .await
        .flatten();
        tracing::info!(ssid = %ssid.as_str(), "connect past snapshot");
        let refresh: Arc<dyn Fn() + Send + Sync> = Arc::new({
            let refresher = ctx.refresher.clone();
            move || refresher.request(nm_client::signals::Cmd::Refresh)
        });
        let outcome = match kind {
            ConnectKind::Open => {
                tokio::time::timeout(CONNECT_TIMEOUT, ctx.client.connect_open(ssid.as_str())).await
            }
            ConnectKind::Saved => {
                tokio::time::timeout(CONNECT_TIMEOUT, ctx.client.connect_saved(ssid.as_str())).await
            }
            ConnectKind::Secure(psk) => {
                tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    ctx.client.connect_secure(ssid.as_str(), psk, &ctx.secrets),
                )
                .await
            }
            ConnectKind::RetrySaved(psk) => {
                tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    ctx.client
                        .retry_saved_with_psk(ssid.as_str(), psk, &ctx.secrets),
                )
                .await
            }
            ConnectKind::Hidden(psk) => {
                tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    ctx.client.connect_hidden(ssid.as_str(), psk, &ctx.secrets),
                )
                .await
            }
        };
        match outcome {
            Ok(Ok(o)) => {
                ctx.client
                    .watch_attempt(
                        &ssid,
                        &o.active,
                        prev,
                        o.created,
                        o.commit,
                        o.persist_on_success,
                        &ctx.secrets,
                        &ctx.model_feed,
                        &ctx.watch_rx,
                        &refresh,
                    )
                    .await;
            }
            Ok(Err(e)) => {
                ctx.scan_frozen.store(false, Ordering::Relaxed);
                tracing::warn!("connect {} failed: {e:#}", ssid);
                if let Some(p) = prev
                    && p.ssid.as_str() != ssid.as_str()
                {
                    let _ = ctx.client.reactivate_previous(&p.ssid, &p.profile).await;
                }
                let _ = ctx.model_feed.try_send(state::UiEvent::SsidError {
                    ssid: ssid.clone(),
                    message: nm_client::connect::MSG_FAILED.into(),
                });
            }
            Err(_) => {
                ctx.scan_frozen.store(false, Ordering::Relaxed);
                tracing::warn!("connect {} timed out", ssid);
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    ctx.client.dispose_stuck_activation(ssid.as_str()),
                )
                .await;
                if let Some(p) = prev
                    && p.ssid.as_str() != ssid.as_str()
                {
                    let _ = ctx.client.reactivate_previous(&p.ssid, &p.profile).await;
                }
                let _ = ctx.model_feed.try_send(state::UiEvent::SsidError {
                    ssid: ssid.clone(),
                    message: nm_client::connect::MSG_FAILED.into(),
                });
            }
        }
        ctx.scan_frozen.store(false, Ordering::Relaxed);
        ctx.refresher.request(nm_client::signals::Cmd::Refresh);
    }
    let mut last_scan_done: Option<std::time::Instant> = None;
    loop {
        tokio::select! {
            _ = shutdown_signal() => {
                info!("shutdown signal received");
                break;
            }
            ev = tray_ev_rx.recv() => {
                let cmd = match ev {
                    Ok(cmd) => cmd,
                    Err(_) => break,
                };
                match cmd {
                    state::BackendCmd::Rescan => {
                        let frozen = scan_frozen.load(Ordering::Relaxed);
                        let fresh = last_scan_done.is_some_and(|t| {
                            t.elapsed() < std::time::Duration::from_secs(30)
                        });
                        if !frozen && !fresh {
                            let mut scanned =
                                with_bus_timeout(20, "manual scan", scan_once_now(&client))
                                    .await
                                    .is_some_and(|ok| ok);
                            if !scanned {
                                tracing::info!(
                                    "scan not accepted; retrying after the NM scan window"
                                );
                                tokio::time::sleep(std::time::Duration::from_secs(11)).await;
                                scanned =
                                    with_bus_timeout(20, "manual scan retry", scan_once_now(&client))
                                        .await
                                        .is_some_and(|ok| ok);
                            }
                            if scanned {
                                last_scan_done = Some(std::time::Instant::now());
                            } else {
                                tracing::info!("manual scan did not run");
                            }
                        } else {
                            refresher.request(nm_client::signals::Cmd::Scan);
                        }
                        settle!();
                    }
                    state::BackendCmd::Refresh => {
                        refresher.request(nm_client::signals::Cmd::Refresh);
                    }
                    state::BackendCmd::SetWifi(on) => {
                        info!(on, "Wi-Fi toggle requested");
                        if let Some(Err(e)) = with_bus_timeout(10, "Wi-Fi toggle", async {
                            client.set_wireless_enabled(on).await
                        })
                        .await
                        {
                            tracing::warn!("set_wireless_enabled failed: {e:#}");
                        }
                        settle!();
                    }
                    state::BackendCmd::SetAirplane(on) => {
                        info!(on, "airplane mode requested");
                        if let Some(Err(e)) = with_bus_timeout(10, "airplane toggle", async {
                            client.set_airplane_mode(on).await
                        })
                        .await
                        {
                            tracing::warn!("set_airplane_mode failed: {e:#}");
                        }
                        settle!();
                    }
                    state::BackendCmd::ConnectOpen(ssid) => {
                        info!(ssid = %ssid.as_str(), "connect requested (open)");
                        guarded_connect(ctx.clone(), ssid, ConnectKind::Open).await;
                    }
                    state::BackendCmd::ConnectSaved(ssid) => {
                        info!(ssid = %ssid.as_str(), "connect requested (saved profile)");
                        guarded_connect(ctx.clone(), ssid, ConnectKind::Saved).await;
                    }
                    state::BackendCmd::ConnectSecure { ssid, psk } => {
                        info!(ssid = %ssid.as_str(), "connect requested (inline password)");
                        guarded_connect(ctx.clone(), ssid, ConnectKind::Secure(psk)).await;
                    }
                    state::BackendCmd::DisconnectActive => {
                        info!("disconnect requested");
                        *last_manual_disconnect.lock().await = Some(std::time::Instant::now());
                        if let Some(Err(e)) = with_bus_timeout(10, "disconnect", async {
                            client.disconnect_active().await
                        })
                        .await
                        {
                            tracing::warn!("disconnect failed: {e:#}");
                        }
                        settle!();
                    }
                    state::BackendCmd::ActivateVpn(id) => {
                        info!(id, "vpn activate requested");
                        if let Err(e) = client.activate_vpn(&id).await {
                            tracing::warn!("activate vpn {id} failed: {e:#}");
                            let _ = model_feed.try_send(state::UiEvent::SsidError {
                                ssid: id.clone().into(),
                                message: "Couldn't start VPN".into(),
                            });
                        }
                        settle!();
                    }
                    state::BackendCmd::DeactivateVpn(id) => {
                        info!(id, "vpn deactivate requested");
                        if let Err(e) = client.deactivate_vpn(&id).await {
                            tracing::warn!("deactivate vpn {id} failed: {e:#}");
                        }
                        settle!();
                    }
                    state::BackendCmd::ProvideSecret { ssid, path, psk } => {
                        info!(%ssid, %path, "retry password provided");
                        if let Err(e) = nm_client::connect::validate_psk(psk.as_str()) {
                            tracing::warn!("rejected invalid retry password for {path}: {e:#}");
                        } else if secrets.provide(&path, psk.to_string()).await {
                            tracing::info!(%ssid, "live secret request answered");
                        } else {
                            tracing::info!(%ssid, "no pending secret request; retrying profile");
                            guarded_connect(ctx.clone(), ssid, ConnectKind::RetrySaved(psk)).await;
                        }
                        settle!();
                    }
                    state::BackendCmd::RetrySaved { ssid, psk, stale_path } => {
                        info!(%ssid, "retrying saved network with a new password");
                        if let Some(path) = stale_path {
                            secrets.cancel_path(&path).await;
                        }
                        guarded_connect(ctx.clone(), ssid, ConnectKind::RetrySaved(psk)).await;
                    }
                    state::BackendCmd::CancelAttempt(ssid) => {
                        info!(ssid = %ssid.as_str(), "cancelling stuck attempt after rejected password");
                        if with_bus_timeout(10, "stuck-activation disposal", async {
                            client.dispose_stuck_activation(ssid.as_str()).await
                        })
                        .await
                        .unwrap_or(false)
                        {
                            tracing::info!(ssid = %ssid.as_str(), "stuck activation cancelled");
                        }
                        settle!();
                    }
                    state::BackendCmd::CreateHotspot { ssid, psk } => {
                        info!(ssid = %ssid.as_str(), "hotspot requested");
                        scan_frozen.store(true, Ordering::Relaxed);
                        match with_bus_timeout(30, "hotspot creation", async {
                            client.create_hotspot_with(ssid.as_str(), psk.as_str()).await
                        })
                        .await
                        {
                            Some(Ok(_created)) => {
                                hotspot_expected.store(true, Ordering::Relaxed);
                                tokio::spawn({
                                    let client = client.clone();
                                    let refresher = refresher.clone();
                                    let hotspot_expected = hotspot_expected.clone();
                                    let model_feed = model_feed.clone();
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
                                            "Couldn't start hotspot".into(),
                                        ));
                                        refresher.request(nm_client::signals::Cmd::Refresh);
                                    }
                                });
                            }
                            Some(Err(e)) => {
                                tracing::warn!("create_hotspot failed: {e:#}");
                                let _ = model_feed.try_send(state::UiEvent::BackendError(
                                    "Couldn't start hotspot".into(),
                                ));
                            }
                            None => {
                                let _ = model_feed.try_send(state::UiEvent::BackendError(
                                    "Couldn't start hotspot".into(),
                                ));
                            }
                        }
                        scan_frozen.store(false, Ordering::Relaxed);
                        settle!();
                    }
                    state::BackendCmd::StopHotspot => {
                        info!("hotspot stop requested");
                        hotspot_expected.store(false, Ordering::Relaxed);
                        if let Some(Err(e)) = with_bus_timeout(10, "hotspot stop", async {
                            client.stop_hotspot().await
                        })
                        .await
                        {
                            tracing::warn!("stop_hotspot failed: {e:#}");
                            let _ = model_feed.try_send(state::UiEvent::BackendError(
                                "Couldn't stop hotspot".into(),
                            ));
                        }
                        settle!();
                    }
                    state::BackendCmd::Forget(ssid) => {
                        info!(ssid = %ssid.as_str(), "forget saved network requested");
                        let paths = with_bus_timeout(10, "active-path lookup", async {
                            client.active_paths_for_ssid(ssid.as_str()).await
                        })
                        .await
                        .unwrap_or_default();
                        for path in paths {
                            secrets.cancel_path(&path.to_string()).await;
                        }
                        let nostuck = with_bus_timeout(10, "stuck-activation disposal", async {
                            client.dispose_stuck_activation(ssid.as_str()).await
                        })
                        .await
                        .unwrap_or(false);
                        if nostuck {
                            tracing::info!(ssid = %ssid.as_str(), "cancelled a stuck activation for the forgotten network");
                        }
                        let cleaned = with_bus_timeout(10, "unsaved cleanup", async {
                            client.delete_unsaved_for_ssid(ssid.as_str()).await
                        })
                        .await
                        .unwrap_or(false);
                        match with_bus_timeout(10, "profile deletion", async {
                            client.forget_saved(ssid.as_str()).await
                        })
                        .await
                        {
                            Some(Ok(())) => tracing::info!(
                                ssid = %ssid.as_str(),
                                "saved profile deleted"
                            ),
                            _ => {
                                let still_saved = with_bus_timeout(10, "saved lookup", async {
                                    nm_client::saved_profile_path(
                                        client.system_conn(),
                                        ssid.as_str(),
                                    )
                                    .await
                                    .ok()
                                    .flatten()
                                    .is_some()
                                })
                                .await
                                .unwrap_or(false);
                                if still_saved {
                                    tracing::warn!(
                                        "forget {ssid} failed: saved profile remains"
                                    );
                                    let _ = model_feed.try_send(state::UiEvent::SsidError {
                                        ssid: ssid.clone(),
                                        message: "Couldn't forget this network".into(),
                                    });
                                } else if cleaned {
                                    tracing::info!(
                                        ssid = %ssid.as_str(),
                                        "leftover unsaved profile removed"
                                    );
                                } else {
                                    tracing::debug!(
                                        ssid = %ssid.as_str(),
                                        "nothing to forget"
                                    );
                                }
                            }
                        }
                        settle!();
                    }
                    state::BackendCmd::ConnectHidden { ssid, psk } => {
                        info!(ssid = %ssid.as_str(), "hidden network connect requested");
                        guarded_connect(ctx.clone(), ssid, ConnectKind::Hidden(psk)).await;
                    }
                    state::BackendCmd::ExpandRow(ssid) => {
                        info!(ssid = %ssid.as_str(), "expand row requested");
                        let _ = model_feed.try_send(state::UiEvent::ExpandRow(ssid));
                    }
                    state::BackendCmd::RefreshTray => {
                        info!("theme change, re-emitting tray icon");
                        refresher.request(nm_client::signals::Cmd::Refresh);
                    }
                    state::BackendCmd::Quit => break,
                }
            }
        }
    }
    feed_task.abort();
    tray_bridge.abort();
    let _ = tray_bridge.await;
    popup_manager.abort();
    for handle in signal_watch {
        handle.abort();
    }
    refresher.stop();
    tray_service.stop();
}
#[cfg(test)]
mod tests {
    use super::*;
    fn a(v: &[&str]) -> Vec<String> {
        std::iter::once("rnetapplet".to_string())
            .chain(v.iter().map(|s| s.to_string()))
            .collect()
    }
    #[test]
    fn accepts_no_arguments() {
        assert!(validate_args(&a(&[])).is_ok());
    }
    #[test]
    fn accepts_every_documented_flag() {
        assert!(validate_args(&a(&["--dump-aps"])).is_ok());
        assert!(validate_args(&a(&["--connect-saved", "Home"])).is_ok());
        assert!(validate_args(&a(&["--wifi", "off"])).is_ok());
        assert!(validate_args(&a(&["--camera-test"])).is_ok());
        assert!(validate_args(&a(&["--hotspot-test"])).is_ok());
        assert!(validate_args(&a(&["--no-auto-failover"])).is_ok());
    }
    #[test]
    fn rejects_unknown_options() {
        assert!(validate_args(&a(&["--dump"])).is_err());
        assert!(validate_args(&a(&["--rescan"])).is_err());
    }
    #[test]
    fn rejects_a_valued_flag_without_its_value() {
        assert!(validate_args(&a(&["--wifi"])).is_err());
        assert!(validate_args(&a(&["--connect-saved"])).is_err());
    }
    #[test]
    fn foreign_pids_are_not_treated_as_ours() {
        assert!(!process_is_rnetapplet(1));
        assert!(!process_is_rnetapplet(u32::MAX));
    }
    #[test]
    fn does_not_treat_a_value_as_a_flag() {
        assert!(validate_args(&a(&["--connect-saved", "--not-a-flag"])).is_ok());
    }
}
