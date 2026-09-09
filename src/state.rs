use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq)]
pub struct Ap {
    pub ssid: String,
    pub bssid_path: String,
    pub strength: u8,
    pub secured: bool,

    pub saved: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VpnConnection {
    pub id: String,
    pub path: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HotspotInfo {
    pub ssid: String,

    pub psk: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub aps: Vec<Ap>,
    pub active_ssid: Option<String>,
    pub active_iface: Option<String>,
    pub active_ipv4: Option<String>,
    pub active_bitrate_kbps: Option<u32>,
    pub hotspot: Option<HotspotInfo>,
    pub wifi_enabled: bool,
    pub networking_enabled: bool,

    pub nm_online: bool,
    pub vpn_connections: Vec<VpnConnection>,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            aps: Vec::new(),
            active_ssid: None,
            active_iface: None,
            active_ipv4: None,
            active_bitrate_kbps: None,
            hotspot: None,
            wifi_enabled: true,
            networking_enabled: true,
            nm_online: true,
            vpn_connections: Vec::new(),
        }
    }
}

impl Model {
    pub fn airplane_mode(&self) -> bool {
        !self.networking_enabled
    }

    pub fn sorted_aps(&self) -> Vec<Ap> {
        let mut aps = self.aps.clone();
        aps.sort_by(|a, b| b.strength.cmp(&a.strength));
        aps
    }
}

pub type ModelTx = watch::Sender<Model>;
pub type ModelRx = watch::Receiver<Model>;

pub fn channel() -> (ModelTx, ModelRx) {
    watch::channel(Model::default())
}

#[derive(Debug, Clone)]
pub enum BackendCmd {
    TogglePopup(Option<(i32, i32)>),
    SetWifi(bool),

    ConnectOpen(String),

    ConnectSaved(String),

    ConnectSecure { ssid: String, psk: String },

    #[allow(dead_code)]
    ProvideSecret { path: String, psk: String },

    SetAirplane(bool),

    CreateHotspot { ssid: String, psk: String },

    StopHotspot,

    Forget(String),

    ConnectHidden { ssid: String, psk: String },

    RefreshTray,
    DisconnectActive,
    Quit,
}

#[derive(Debug, Clone)]
pub enum UiEvent {
    Model(Model),

    BackendError(String),

    SsidError { ssid: String, message: String },

    SecretsNeeded { ssid: String, path: String, request_new: bool },

    Speeds { up_bps: Option<u64>, down_bps: Option<u64> },
}

pub fn format_rate(bps: Option<u64>) -> String {
    match bps {
        None => String::from("…"),
        Some(b) if b < 1024 => format!("{b} B/s"),
        Some(b) if b < 1024 * 1024 => format!("{:.1} KB/s", b as f64 / 1024.0),
        Some(b) => format!("{:.1} MB/s", b as f64 / 1048576.0),
    }
}
