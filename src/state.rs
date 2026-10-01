use std::sync::Arc;
use tokio::sync::watch;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ap {
    pub ssid: String,
    pub strength: u8,
    pub secured: bool,
    pub saved: bool,
    pub priority: i32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpnConnection {
    pub id: String,
    pub active: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotspotInfo {
    pub ssid: String,
    pub psk: Option<String>,
    pub active: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Model {
    pub aps: Arc<[Ap]>,
    pub active_ssid: Option<String>,
    pub active_iface: Option<String>,
    pub active_ipv4: Option<String>,
    pub active_bitrate_kbps: Option<u32>,
    pub hotspot: Option<HotspotInfo>,
    pub wifi_enabled: bool,
    pub networking_enabled: bool,
    pub nm_online: bool,
    pub vpn_connections: Arc<[VpnConnection]>,
}
impl Model {
    pub fn empty() -> Self {
        Self {
            aps: Arc::from(Vec::new()),
            vpn_connections: Arc::from(Vec::new()),
            wifi_enabled: true,
            networking_enabled: true,
            nm_online: true,
            ..Default::default()
        }
    }
    pub fn airplane_mode(&self) -> bool {
        !self.networking_enabled
    }
    pub fn active_strength(&self) -> Option<u8> {
        let ssid = self.active_ssid.as_deref()?;
        self.aps
            .iter()
            .find(|ap| ap.ssid == ssid)
            .map(|ap| ap.strength)
    }
}
pub type ModelTx = watch::Sender<Arc<Model>>;
pub type ModelRx = watch::Receiver<Arc<Model>>;
pub fn channel() -> (ModelTx, ModelRx) {
    watch::channel(Arc::new(Model::empty()))
}
#[derive(Debug, Clone)]
pub enum BackendCmd {
    TogglePopup(Option<(i32, i32)>),
    Rescan,

    Refresh,
    ConnectOpen(String),
    ConnectSaved(String),
    ConnectSecure {
        ssid: String,
        psk: String,
    },
    #[allow(dead_code)]
    ProvideSecret {
        path: String,
        psk: String,
    },
    SetAirplane(bool),
    CreateHotspot {
        ssid: String,
        psk: String,
    },
    StopHotspot,
    Forget(String),
    ConnectHidden {
        ssid: String,
        psk: String,
    },
    SetWifi(bool),
    RefreshTray,
    DisconnectActive,
    ActivateVpn(String),
    DeactivateVpn(String),
    Quit,
}
#[derive(Debug, Clone)]
pub enum UiEvent {
    Model(Arc<Model>),
    BackendError(String),
    SsidError {
        ssid: String,
        message: String,
    },
    SecretsNeeded {
        ssid: String,
        path: String,
        request_new: bool,
    },
    Speeds {
        up_bps: Option<u64>,
        down_bps: Option<u64>,
    },
}
pub fn format_rate(bps: Option<u64>) -> String {
    match bps {
        None => String::from("…"),
        Some(b) if b < 1024 => format!("{b} B/s"),
        Some(b) if b < 1024 * 1024 => format!("{:.1} KB/s", b as f64 / 1024.0),
        Some(b) => format!("{:.1} MB/s", b as f64 / 1048576.0),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn ap(ssid: &str, strength: u8) -> Ap {
        Ap {
            priority: 0,
            ssid: ssid.into(),
            strength,
            secured: true,
            saved: false,
        }
    }
    #[test]
    fn empty_model_is_online() {
        let m = Model::empty();
        assert!(m.nm_online);
        assert!(m.wifi_enabled);
        assert!(m.aps.is_empty());
    }
    #[test]
    fn clone_shares_ap_list() {
        let m = Model {
            aps: Arc::from(vec![ap("Home", 80)]),
            ..Model::empty()
        };
        let c = m.clone();
        assert!(Arc::ptr_eq(&m.aps, &c.aps));
    }
    #[test]
    fn active_strength_lookup() {
        let m = Model {
            aps: Arc::from(vec![ap("Home", 63), ap("Cafe", 21)]),
            active_ssid: Some("Cafe".into()),
            ..Model::empty()
        };
        assert_eq!(m.active_strength(), Some(21));
    }
    #[test]
    fn rate_formatting() {
        assert_eq!(format_rate(None), "…");
        assert_eq!(format_rate(Some(512)), "512 B/s");
        assert_eq!(format_rate(Some(2048)), "2.0 KB/s");
        assert_eq!(format_rate(Some(3 * 1024 * 1024)), "3.0 MB/s");
    }
}
