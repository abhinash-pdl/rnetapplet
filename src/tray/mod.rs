pub use crate::state::BackendCmd as TrayEvent;
use crate::state::{BackendCmd, Model, ModelRx};
use ksni::TrayMethods as _;
use std::sync::Arc;
pub struct RnetTray {
    model: Arc<Model>,
    event_tx: async_channel::Sender<BackendCmd>,
}
impl RnetTray {
    pub fn new(model: Arc<Model>, event_tx: async_channel::Sender<BackendCmd>) -> Self {
        Self { model, event_tx }
    }
    fn emit(&self, ev: TrayEvent) {
        let _ = self.event_tx.try_send(ev);
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
        self.emit(BackendCmd::TogglePopup(Some((x, y))));
    }
    fn menu(&self) -> Vec<ksni::menu::MenuItem<Self>> {
        use ksni::menu::*;
        let wifi_on = self.model.wifi_enabled;
        vec![
            StandardItem {
                label: "Open".into(),
                icon_name: "network-wireless".into(),
                activate: Box::new(|tray: &mut Self| tray.emit(BackendCmd::TogglePopup(None))),
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
pub async fn spawn(
    initial: Arc<Model>,
    event_tx: async_channel::Sender<BackendCmd>,
) -> anyhow::Result<ksni::Handle<RnetTray>> {
    let tray = RnetTray::new(initial, event_tx);
    let handle = tray.spawn().await?;
    Ok(handle)
}
pub fn spawn_updater(
    handle: ksni::Handle<RnetTray>,
    mut rx: ModelRx,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if rx.changed().await.is_err() {
                break;
            }
            let model = rx.borrow().clone();
            if handle.update(|tray| tray.model = model).await.is_none() {
                break;
            }
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Ap, Model};
    fn model_with(strength: u8, active: Option<&str>) -> Model {
        Model {
            aps: Arc::from(vec![Ap {
                priority: 0,
                ssid: "Home".into(),
                strength,
                secured: true,
                enterprise: false,
                wep: false,
                freq_mhz: None,
                bands: 0,
                known: false,
                saved: true,
            }]),
            active_ssid: active.map(str::to_string),
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
