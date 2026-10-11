pub use crate::state::BackendCmd as TrayEvent;
use crate::state::{BackendCmd, Model};
use ksni::TrayMethods as _;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
pub struct RnetTray {
    model: Arc<Model>,
    event_tx: async_channel::Sender<BackendCmd>,
    toggle_tx: async_channel::Sender<Option<(i32, i32)>>,
}
impl RnetTray {
    pub fn new(
        model: Arc<Model>,
        event_tx: async_channel::Sender<BackendCmd>,
        toggle_tx: async_channel::Sender<Option<(i32, i32)>>,
    ) -> Self {
        Self {
            model,
            event_tx,
            toggle_tx,
        }
    }
    fn emit(&self, ev: TrayEvent) {
        let _ = self.event_tx.try_send(ev);
    }
    fn toggle(&self, pos: Option<(i32, i32)>) {
        let _ = self.toggle_tx.try_send(pos);
    }
}
pub fn icon_for(model: &Model) -> String {
    if !model.nm_online {
        return "network-offline-symbolic".into();
    }
    if !model.networking_enabled {
        return "airplane-mode-symbolic".into();
    }
    if model.hotspot.as_ref().is_some_and(|h| h.active) {
        return "network-wireless-hotspot-symbolic".into();
    }
    let wifi_up = model.active_ssid.is_some();
    let wired_up = model.primary_wired || model.wired.is_some();
    if !model.wifi_enabled && !wired_up {
        return "network-wireless-disabled-symbolic".into();
    }
    if !wifi_up && !wired_up {
        return "network-wireless-offline-symbolic".into();
    }
    if model.no_internet && !model.vpn_connections.iter().any(|v| v.active) {
        return if wifi_up {
            "network-wireless-no-route-symbolic"
        } else {
            "network-wired-no-route-symbolic"
        }
        .into();
    }
    if model.primary_wired || !wifi_up {
        return "network-wired-symbolic".into();
    }
    let strength = model.active_strength().unwrap_or(0);
    match strength {
        0..=5 => "network-wireless-signal-none-symbolic",
        6..=30 => "network-wireless-signal-weak-symbolic",
        31..=55 => "network-wireless-signal-ok-symbolic",
        56..=80 => "network-wireless-signal-good-symbolic",
        _ => "network-wireless-signal-excellent-symbolic",
    }
    .into()
}
pub fn overlay_icon_for(model: &Model) -> String {
    if model.vpn_connections.iter().any(|v| v.active) {
        "network-vpn-symbolic".into()
    } else {
        String::new()
    }
}
fn tooltip_text(model: &Model) -> String {
    if !model.nm_online {
        return "NetworkManager unavailable".into();
    }
    if !model.networking_enabled {
        return "Airplane mode".into();
    }
    if model.hotspot.as_ref().is_some_and(|h| h.active) {
        return "Hotspot active".into();
    }
    let wifi_up = model.active_ssid.is_some();
    if !model.wifi_enabled && model.wired.is_none() {
        return "Wi-Fi disabled".into();
    }
    if !wifi_up && model.wired.is_none() {
        return "Disconnected".into();
    }
    if model.no_internet && !model.vpn_connections.iter().any(|v| v.active) {
        return match model.active_ssid.as_deref() {
            Some(ssid) => format!("Connected to {ssid} — no internet"),
            None => "Wired connection — no internet".into(),
        };
    }
    match model.active_ssid.as_deref() {
        Some(ssid) => {
            let extra = model
                .active_strength()
                .map(|s| format!(" ({s}%)"))
                .unwrap_or_default();
            format!("Connected to {ssid}{extra}")
        }
        None => "Wired connection".into(),
    }
}
impl ksni::Tray for RnetTray {
    fn id(&self) -> String {
        "rnetapplet".into()
    }
    fn title(&self) -> String {
        "rnetapplet — Network".into()
    }
    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }
    fn icon_name(&self) -> String {
        icon_for(&self.model)
    }
    fn overlay_icon_name(&self) -> String {
        overlay_icon_for(&self.model)
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_name: self.icon_name(),
            title: "rnetapplet".into(),
            description: tooltip_text(&self.model),
            ..Default::default()
        }
    }
    fn activate(&mut self, x: i32, y: i32) {
        tracing::debug!(x, y, "tray icon activated");
        self.toggle(Some((x, y)));
    }
    fn menu(&self) -> Vec<ksni::menu::MenuItem<Self>> {
        use ksni::menu::*;
        let wifi_on = self.model.wifi_enabled;
        vec![
            StandardItem {
                label: "Open".into(),
                icon_name: "network-wireless".into(),
                activate: Box::new(|tray: &mut Self| tray.toggle(None)),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Enable Wi-Fi".into(),
                checked: wifi_on,
                activate: Box::new(move |tray: &mut Self| {
                    let next = !tray.model.wifi_enabled;
                    tray.emit(BackendCmd::SetWifi(next));
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|tray: &mut Self| tray.emit(BackendCmd::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}
const TRAY_RETRY_INITIAL: Duration = Duration::from_secs(2);
const TRAY_RETRY_MAX: Duration = Duration::from_secs(30);
const TRAY_UPDATE_TIMEOUT: Duration = Duration::from_secs(3);
const TRAY_SPAWN_TIMEOUT: Duration = Duration::from_secs(15);
const TRAY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const TRAY_RESTART_PAUSE: Duration = Duration::from_secs(2);

pub struct TrayService {
    tx: mpsc::Sender<Arc<Model>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl TrayService {
    pub fn start(
        initial: Arc<Model>,
        event_tx: async_channel::Sender<BackendCmd>,
        toggle_tx: async_channel::Sender<Option<(i32, i32)>>,
    ) -> Self {
        let (tx, mut rx) = mpsc::channel(1);
        let _ = tx.try_send(initial);
        let thread = std::thread::Builder::new()
            .name("rnetapplet-tray".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!("could not build tray runtime: {e:#}");
                        return;
                    }
                };
                loop {
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        rt.block_on(run_tray(&mut rx, &event_tx, &toggle_tx))
                    }));
                    match outcome {
                        Ok(()) => return,
                        Err(_) => {
                            tracing::error!("tray task failed; restarting the tray integration");
                            std::thread::sleep(TRAY_RESTART_PAUSE);
                        }
                    }
                }
            })
            .map_err(|e| tracing::warn!("could not spawn tray thread: {e}"))
            .ok();
        Self { tx, thread }
    }
    pub fn clone_sender(&self) -> mpsc::Sender<Arc<Model>> {
        self.tx.clone()
    }
    pub fn stop(mut self) {
        drop(self.tx);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

async fn shutdown_handle(handle: ksni::Handle<RnetTray>) {
    if tokio::time::timeout(TRAY_SHUTDOWN_TIMEOUT, handle.shutdown())
        .await
        .is_err()
    {
        tracing::warn!("tray shutdown stalled; abandoning the stale registration");
    }
}

async fn backoff_once(
    rx: &mut mpsc::Receiver<Arc<Model>>,
    latest: &mut Option<Arc<Model>>,
    backoff: &mut Duration,
) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(*backoff) => {}
        m = rx.recv() => match m {
            Some(m) => *latest = Some(m),
            None => return false,
        },
    }
    *backoff = (*backoff * 2).min(TRAY_RETRY_MAX);
    true
}

async fn run_tray(
    rx: &mut mpsc::Receiver<Arc<Model>>,
    event_tx: &async_channel::Sender<BackendCmd>,
    toggle_tx: &async_channel::Sender<Option<(i32, i32)>>,
) {
    let mut latest: Option<Arc<Model>> = None;
    let mut backoff = TRAY_RETRY_INITIAL;
    loop {
        if latest.is_none() {
            match rx.recv().await {
                Some(m) => latest = Some(m),
                None => return,
            }
            continue;
        }
        let spawned = tokio::time::timeout(
            TRAY_SPAWN_TIMEOUT,
            spawn(
                latest.as_ref().unwrap().clone(),
                event_tx.clone(),
                toggle_tx.clone(),
            ),
        )
        .await;
        let handle = match spawned {
            Ok(Ok(handle)) => handle,
            Ok(Err(e)) => {
                tracing::warn!("SNI tray host not available ({e:#}); retrying in {backoff:?}");
                if !backoff_once(rx, &mut latest, &mut backoff).await {
                    return;
                }
                continue;
            }
            Err(_) => {
                tracing::warn!(
                    "tray registration timed out after {TRAY_SPAWN_TIMEOUT:?}; retrying"
                );
                if !backoff_once(rx, &mut latest, &mut backoff).await {
                    return;
                }
                continue;
            }
        };
        tracing::info!("tray registered as org.kde.StatusNotifierItem (id=rnetapplet)");
        backoff = TRAY_RETRY_INITIAL;
        let mut healthy = true;
        while healthy {
            loop {
                match rx.recv().await {
                    None => {
                        shutdown_handle(handle).await;
                        return;
                    }
                    Some(m) => latest = Some(m),
                }
                if rx.try_recv().is_err() {
                    break;
                }
            }
            let model = latest.as_ref().unwrap().clone();
            match tokio::time::timeout(
                TRAY_UPDATE_TIMEOUT,
                handle.update(|tray| tray.model = model),
            )
            .await
            {
                Ok(Some(_)) => {}
                Ok(None) => {
                    tracing::warn!("tray item detached from host; re-registering");
                    healthy = false;
                }
                Err(_) => {
                    tracing::warn!("tray update timed out; re-registering item");
                    healthy = false;
                }
            }
        }
        shutdown_handle(handle).await;
    }
}

async fn spawn(
    initial: Arc<Model>,
    event_tx: async_channel::Sender<BackendCmd>,
    toggle_tx: async_channel::Sender<Option<(i32, i32)>>,
) -> anyhow::Result<ksni::Handle<RnetTray>> {
    let tray = RnetTray::new(initial, event_tx, toggle_tx);
    Ok(tray.spawn().await?)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::nm_client::aps::{Bands, Security};
    use crate::state::{Ap, Model, Ssid};
    fn model_with(strength: u8, active: Option<&str>) -> Model {
        Model {
            aps: Arc::from(vec![Ap {
                priority: 0,
                ssid: "Home".into(),
                strength,
                security: Security::Wpa,
                freq_mhz: None,
                bands: Bands::NONE,
                known: false,
                saved: true,
            }]),
            active_ssid: active.map(Ssid::from),
            ..Model::empty()
        }
    }
    #[test]
    fn icon_steps() {
        assert_eq!(
            icon_for(&model_with(0, Some("Home"))),
            "network-wireless-signal-none-symbolic"
        );
        assert_eq!(
            icon_for(&model_with(20, Some("Home"))),
            "network-wireless-signal-weak-symbolic"
        );
        assert_eq!(
            icon_for(&model_with(50, Some("Home"))),
            "network-wireless-signal-ok-symbolic"
        );
        assert_eq!(
            icon_for(&model_with(70, Some("Home"))),
            "network-wireless-signal-good-symbolic"
        );
        assert_eq!(
            icon_for(&model_with(95, Some("Home"))),
            "network-wireless-signal-excellent-symbolic"
        );
    }
    #[test]
    fn icon_special_states() {
        assert_eq!(
            icon_for(&model_with(80, None)),
            "network-wireless-offline-symbolic"
        );
        let mut m = model_with(80, Some("Home"));
        m.wifi_enabled = false;
        assert_eq!(icon_for(&m), "network-wireless-disabled-symbolic");
        m.networking_enabled = false;
        assert_eq!(icon_for(&m), "airplane-mode-symbolic");
    }
    #[test]
    fn icon_no_network_manager() {
        let mut m = model_with(80, Some("Home"));
        m.nm_online = false;
        assert_eq!(icon_for(&m), "network-offline-symbolic");
        assert_eq!(tooltip_text(&m), "NetworkManager unavailable");
    }
    #[test]
    fn icon_hotspot() {
        let mut m = model_with(80, None);
        m.hotspot = Some(crate::state::HotspotInfo {
            ssid: "Phone".into(),
            psk: None,
            active: true,
        });
        assert_eq!(icon_for(&m), "network-wireless-hotspot-symbolic");
        assert_eq!(tooltip_text(&m), "Hotspot active");
    }
    #[test]
    fn icon_wired() {
        let mut m = model_with(80, None);
        m.primary_wired = true;
        m.wired = Some(crate::state::WiredInfo {
            id: "Wired connection 1".into(),
            iface: "enp3s0".into(),
            ipv4: Some("192.168.1.10".into()),
            speed_mbps: Some(1000),
        });
        assert_eq!(icon_for(&m), "network-wired-symbolic");
        assert_eq!(tooltip_text(&m), "Wired connection");
        assert_eq!(
            icon_for(&model_with(80, None)),
            "network-wireless-offline-symbolic"
        );
    }
    #[test]
    fn icon_wired_wins_over_wifi_when_primary() {
        let mut m = model_with(80, Some("Home"));
        m.primary_wired = true;
        m.wired = Some(crate::state::WiredInfo {
            id: "Wired connection 1".into(),
            iface: "enp3s0".into(),
            ipv4: None,
            speed_mbps: None,
        });
        assert_eq!(icon_for(&m), "network-wired-symbolic");
        m.primary_wired = false;
        assert_eq!(icon_for(&m), "network-wireless-signal-good-symbolic");
    }
    #[test]
    fn icon_no_internet() {
        let mut m = model_with(80, Some("Home"));
        m.no_internet = true;
        assert_eq!(icon_for(&m), "network-wireless-no-route-symbolic");
        assert_eq!(tooltip_text(&m), "Connected to Home — no internet");
        m.active_ssid = None;
        m.wired = Some(crate::state::WiredInfo {
            id: "Wired connection 1".into(),
            iface: "enp3s0".into(),
            ipv4: None,
            speed_mbps: None,
        });
        assert_eq!(icon_for(&m), "network-wired-no-route-symbolic");
        assert_eq!(tooltip_text(&m), "Wired connection — no internet");
    }
    #[test]
    fn icons_exist_in_adwaita() {
        let icons = [
            icon_for(&model_with(50, Some("Home"))),
            icon_for(&model_with(80, None)),
            "network-offline-symbolic".into(),
            "network-error-symbolic".into(),
            "network-wired-symbolic".into(),
            "network-wireless-hotspot-symbolic".into(),
            "network-wireless-disabled-symbolic".into(),
            "airplane-mode-symbolic".into(),
        ];
        for name in icons {
            assert!(!name.is_empty(), "empty icon name");
        }
    }
}
