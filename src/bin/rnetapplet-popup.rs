#[path = "support/nm_client.rs"]
pub mod nm_client;
#[path = "../qr.rs"]
mod qr;
#[path = "../qr_scan.rs"]
mod qr_scan;
#[path = "../state.rs"]
mod state;
#[path = "../ui/mod.rs"]
mod ui;

#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

const HELP: &str = "rnetapplet-popup — NetworkManager popup windows for wlroots compositors
USAGE:
    rnetapplet-popup [FLAGS] [OPTIONS]
FLAGS:
    --popup-child         Run the network popup window (spawned by the tray process)
    --camera-test         Probe the camera + QR decode path and exit
    --scan-qr             Run the QR scanner window and print the result as JSON
    --audit-icons         Check the icon theme for the symbols the popup needs
    -h, --help            Print this help
    -V, --version         Print version
OPTIONS:
    --popup-x <PX>        Popup anchor X (with --popup-child)
    --popup-y <PY>        Popup anchor Y (with --popup-child)
    --parent-pid <PID>    Watchdog parent pid (with --scan-qr)
";

fn take_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn audit_icons() -> anyhow::Result<()> {
    if gtk4::init().is_err() {
        println!("icons: no display, skipped");
        return Ok(());
    }
    let Some(display) = gtk4::gdk::Display::default() else {
        println!("icons: no display, skipped");
        return Ok(());
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
    Ok(())
}

fn main() -> anyhow::Result<()> {
    unsafe {
        libc::prctl(libc::PR_SET_THP_DISABLE, 1, 0, 0, 0);
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rnetapplet=info".into()),
        )
        .with_ansi(false)
        .init();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return Ok(());
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("rnetapplet-popup {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--camera-test") {
        return qr_scan::camera_self_test();
    }
    if args.iter().any(|a| a == "--scan-qr") {
        let parent = take_value(&args, "--parent-pid").and_then(|s| s.parse::<u32>().ok());
        return qr_scan::scanner_child(parent);
    }
    if args.iter().any(|a| a == "--audit-icons") {
        return audit_icons();
    }
    if args.iter().any(|a| a == "--popup-child") {
        let x = take_value(&args, "--popup-x").and_then(|s| s.parse::<i32>().ok());
        let y = take_value(&args, "--popup-y").and_then(|s| s.parse::<i32>().ok());
        return ui::child::run_popup_child(x.zip(y));
    }
    print!("{HELP}");
    Ok(())
}
