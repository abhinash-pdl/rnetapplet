use crate::nm_client::aps::{Bands, Security};
use std::sync::Arc;
use tokio::sync::watch;

#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct Ssid(String);

impl Ssid {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Ssid {
    fn from(inner: String) -> Self {
        Self(inner)
    }
}

impl From<&str> for Ssid {
    fn from(inner: &str) -> Self {
        Self(inner.to_string())
    }
}

impl std::fmt::Display for Ssid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::ops::Deref for Ssid {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for Ssid {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Ssid {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl std::borrow::Borrow<str> for Ssid {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Secret(String);

impl Secret {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for Secret {
    fn from(inner: String) -> Self {
        Self(inner)
    }
}

impl From<&str> for Secret {
    fn from(inner: &str) -> Self {
        Self(inner.to_string())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl std::ops::Deref for Secret {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ap {
    pub ssid: Ssid,
    pub bands: Bands,
    pub strength: u8,
    pub security: Security,
    pub freq_mhz: Option<u32>,
    pub saved: bool,

    pub known: bool,
    pub priority: i32,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VpnConnection {
    pub id: String,
    pub active: bool,

    pub path: String,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HotspotInfo {
    pub ssid: Ssid,
    pub psk: Option<Secret>,
    pub active: bool,
    pub shared_radio: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WiredInfo {
    pub id: String,
    pub iface: String,
    pub ipv4: Option<String>,
    pub speed_mbps: Option<u32>,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Model {
    pub aps: Arc<[Ap]>,
    pub active_ssid: Option<Ssid>,
    pub activating_ssid: Option<Ssid>,
    pub active_iface: Option<String>,
    pub active_ipv4: Option<String>,
    pub active_bitrate_kbps: Option<u32>,
    pub active_gateway: Option<String>,
    pub active_dns: Vec<String>,
    pub active_freq_mhz: Option<u32>,
    pub wired: Option<WiredInfo>,
    pub hotspot: Option<HotspotInfo>,
    pub wifi_enabled: bool,
    pub networking_enabled: bool,
    pub nm_online: bool,
    pub no_internet: bool,
    pub primary_wired: bool,
    pub scanning: bool,
    pub vpn_connections: Arc<[VpnConnection]>,
    pub last_good: Option<Ssid>,
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
    #[allow(dead_code)]
    pub fn airplane_mode(&self) -> bool {
        !self.networking_enabled
    }
    #[allow(dead_code)]
    pub fn active_strength(&self) -> Option<u8> {
        let ssid = self.active_ssid.as_deref()?;
        self.aps
            .iter()
            .find(|ap| ap.ssid == ssid)
            .map(|ap| ap.strength)
    }
}
#[allow(dead_code)]
pub type ModelTx = watch::Sender<Arc<Model>>;
#[allow(dead_code)]
pub type ModelRx = watch::Receiver<Arc<Model>>;
#[allow(dead_code)]
pub fn channel() -> (ModelTx, ModelRx) {
    watch::channel(Arc::new(Model::empty()))
}
pub const MSG_FAILED: &str = "Couldn't connect";
pub const MSG_WRONG_PASSWORD: &str = "Incorrect password";
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum BackendCmd {
    Rescan,

    Refresh,
    ConnectOpen(Ssid),
    ConnectSaved(Ssid),
    ConnectSecure {
        ssid: Ssid,
        psk: Secret,
    },
    ProvideSecret {
        ssid: Ssid,
        path: String,
        psk: Secret,
    },

    RetrySaved {
        ssid: Ssid,
        psk: Secret,
        stale_path: Option<String>,
    },
    CancelAttempt(Ssid),
    SetAirplane(bool),
    CreateHotspot {
        ssid: Ssid,
        psk: Secret,
    },
    StopHotspot,
    Forget(Ssid),
    ConnectHidden {
        ssid: Ssid,
        psk: Secret,
    },
    ExpandRow(Ssid),
    SetWifi(bool),
    RefreshTray,
    DisconnectActive,
    ActivateVpn(String),
    DeactivateVpn(String),
    Quit,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum UiEvent {
    Model(Arc<Model>),
    BackendError(String),
    ExpandRow(Ssid),
    SsidError {
        ssid: Ssid,
        message: String,
    },
    VpnError {
        id: String,
        message: String,
    },
    ClearConnecting(Ssid),
    SecretsNeeded {
        ssid: Ssid,
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

#[allow(dead_code)]
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
            bands: Bands::NONE,
            ssid: ssid.into(),
            strength,
            security: Security::Wpa,
            freq_mhz: None,
            saved: false,
            known: false,
        }
    }
    #[test]
    fn identical_models_compare_equal() {
        let a = Model::empty();
        let b = Model::empty();
        assert_eq!(a, b);
        let mut changed = b;
        changed.no_internet = true;
        assert_ne!(a, changed);
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
