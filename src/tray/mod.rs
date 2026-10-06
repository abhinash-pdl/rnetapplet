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
        return "network-wireless-disconnected-symbolic".into();
    }
    if !model.networking_enabled {
        return "airplane-mode-symbolic".into();
    }
    if !model.wifi_enabled {
        return "network-wireless-disabled-symbolic".into();
    }
    let Some(_active) = model.active_ssid.as_deref() else {
        return "network-wireless-signal-none-symbolic".into();
    };
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
    if !model.wifi_enabled {
        return "Wi-Fi disabled".into();
    }
    match model.active_ssid.as_deref() {
        Some(ssid) => {
            let extra = model
                .active_strength()
                .map(|s| format!(" ({s}%)"))
                .unwrap_or_default();
            format!("Connected to {ssid}{extra}")
        }
        None => "Disconnected".into(),
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
            "network-wireless-signal-none-symbolic"
        );
        let mut m = model_with(80, Some("Home"));
        m.wifi_enabled = false;
        assert_eq!(icon_for(&m), "network-wireless-disabled-symbolic");
        m.networking_enabled = false;
        assert_eq!(icon_for(&m), "airplane-mode-symbolic");
    }
}
