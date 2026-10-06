use std::sync::Arc;
use tokio::sync::watch;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ap {
    pub ssid: String,
    pub bands: u8,
    pub strength: u8,
    pub secured: bool,
    pub enterprise: bool,
    pub wep: bool,
    pub freq_mhz: Option<u32>,
    pub saved: bool,

    pub known: bool,
    pub priority: i32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VpnConnection {
    pub id: String,
    pub active: bool,

    pub path: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotspotInfo {
    pub ssid: String,
    pub psk: Option<String>,
    pub active: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WiredInfo {
    pub id: String,
    pub iface: String,
    pub ipv4: Option<String>,
    pub speed_mbps: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Model {
    pub aps: Arc<[Ap]>,
    pub active_ssid: Option<String>,
    pub active_iface: Option<String>,
    pub active_ipv4: Option<String>,
    pub active_bitrate_kbps: Option<u32>,
    pub active_gateway: Option<String>,
    pub active_dns: Vec<String>,
    pub active_freq_mhz: Option<u32>,
    pub saved_ssids: Arc<[String]>,
    pub wired: Option<WiredInfo>,
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
        ssid: String,
        path: String,
        psk: String,
    },

    RetrySaved {
        ssid: String,
        psk: String,
        stale_path: Option<String>,
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
    ExpandRow(String),
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
    ExpandRow(String),
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

pub fn sort_aps(aps: &mut [Ap]) {
    aps.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| b.strength.cmp(&a.strength))
            .then_with(|| b.saved.cmp(&a.saved))
            .then_with(|| a.ssid.cmp(&b.ssid))
    });
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
            bands: 0,
            ssid: ssid.into(),
            strength,
            secured: true,
            enterprise: false,
            wep: false,
            freq_mhz: None,
            saved: false,
            known: false,
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
    fn rows_are_ordered_by_band_then_signal_then_saved_then_name() {
        let mut aps = vec![
            Ap {
                priority: 1,
                ..ap("OtherBand", 20)
            },
            Ap {
                priority: 0,
                saved: true,
                ..ap("Saved", 30)
            },
            Ap {
                priority: 0,
                ..ap("Loud", 90)
            },
            Ap {
                priority: 0,
                ..ap("Middling", 55)
            },
        ];
        sort_aps(&mut aps);
        let names: Vec<&str> = aps.iter().map(|a| a.ssid.as_str()).collect();
        assert_eq!(
            names,
            vec!["OtherBand", "Loud", "Middling", "Saved"],
            "band comes first, then signal, and saved only breaks a tie"
        );
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
