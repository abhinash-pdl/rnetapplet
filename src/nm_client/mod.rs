pub mod aps;
pub mod connect;
pub mod details;
pub mod hotspot;
pub mod known;
pub mod profiles;
pub mod radio;
pub mod signals;
use crate::state::{Ap, Model, VpnConnection};
use anyhow::{Context, Result};
use futures::StreamExt as _;
use rusty_network_manager::{ActiveProxy, DeviceProxy, NetworkManagerProxy, WirelessProxy};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
pub const NM_DEVICE_TYPE_WIFI: u32 = 2;
const FANOUT: usize = 24;
const SCAN_POLL: Duration = Duration::from_millis(250);
const PROFILE_TTL: Duration = Duration::from_secs(30);
const CONN_BAD_GRACE: Duration = Duration::from_secs(5);
pub const SCAN_WAIT: Duration = Duration::from_secs(12);
const STALE_HORIZON: Duration = Duration::from_secs(120);

pub const DBUS_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

const STALE_TOLERANCE_VISIBLE: u64 = 12;

const STALE_TOLERANCE_HIDDEN: u64 = 86_400;

pub fn boottime_secs() -> Option<u64> {
    let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
    let secs = uptime.split_whitespace().next()?;
    secs.split('.').next()?.parse().ok()
}

pub fn is_ghost(last_seen: u64, scan_completed: u64, tolerance: u64) -> bool {
    last_seen > 0 && scan_completed > 0 && last_seen.saturating_add(tolerance) < scan_completed
}
const AP_INTERFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
pub async fn map_bounded<I, T, F, Fut, R>(items: I, f: F) -> Vec<R>
where
    I: IntoIterator<Item = T>,
    F: Fn(T) -> Fut + Clone,
    Fut: std::future::Future<Output = R>,
{
    futures::stream::iter(items.into_iter().map(f))
        .buffer_unordered(FANOUT)
        .collect()
        .await
}
pub type ProfileIndex = (Vec<VpnConnection>, std::collections::HashMap<String, i32>);

pub struct NmClient {
    system: Connection,
    pub nm: NetworkManagerProxy<'static>,
    profiles: tokio::sync::Mutex<Option<(Instant, Arc<ProfileIndex>)>>,
    scan_completed: AtomicU64,
    connected_once: tokio::sync::RwLock<std::collections::BTreeSet<String>>,
    popup_visible: std::sync::Arc<std::sync::atomic::AtomicBool>,
    conn_bad_since: tokio::sync::Mutex<Option<Instant>>,
}
impl NmClient {
    pub async fn connect() -> Result<Self> {
        let system = zbus::connection::Builder::system()
            .context("system bus address")?
            .method_timeout(DBUS_CALL_TIMEOUT)
            .build()
            .await
            .context("failed to connect to system D-Bus")?;
        let nm = NetworkManagerProxy::new(&system)
            .await
            .context("failed to create NetworkManager proxy")?;
        Ok(Self {
            system,
            nm,
            profiles: tokio::sync::Mutex::new(None),
            scan_completed: AtomicU64::new(0),
            connected_once: tokio::sync::RwLock::new(known::load()),
            popup_visible: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            conn_bad_since: tokio::sync::Mutex::new(None),
        })
    }
    pub fn system_conn(&self) -> &Connection {
        &self.system
    }
    pub fn popup_visible(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.popup_visible.clone()
    }

    pub async fn has_connected(&self, ssid: &str) -> bool {
        self.connected_once.read().await.contains(ssid)
    }
    pub async fn mark_connected(&self, ssid: &str) {
        let mut set = self.connected_once.write().await;
        if set.insert(ssid.to_string()) {
            known::save(&set);
            tracing::info!(ssid, "network marked as previously connected");
        }
    }
    pub async fn version(&self) -> Result<String> {
        Ok(self.nm.version().await?)
    }
    pub async fn wifi_device_path(&self) -> Result<Option<OwnedObjectPath>> {
        let devices = self.nm.get_devices().await?;
        let types = map_bounded(devices, |path| {
            let conn = self.system.clone();
            async move {
                let dev = DeviceProxy::new_from_path(path.clone(), &conn).await?;
                Ok::<_, zbus::Error>((path, dev.device_type().await.unwrap_or(0)))
            }
        })
        .await;
        Ok(types
            .into_iter()
            .flatten()
            .find(|(_, t)| *t == NM_DEVICE_TYPE_WIFI)
            .map(|(p, _)| p))
    }
    pub async fn scan_and_wait(
        &self,
        wifi_path: &OwnedObjectPath,
        timeout: Duration,
    ) -> Result<bool> {
        let wifi = WirelessProxy::new_from_path(wifi_path.clone(), self.system_conn()).await?;
        let before = wifi.last_scan().await.unwrap_or(0);
        let _ = wifi.request_scan(std::collections::HashMap::new()).await;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let now = wifi.last_scan().await.unwrap_or(0);
            if now > 0 && now != before {
                if let Some(done) = boottime_secs() {
                    self.scan_completed.store(done, Ordering::Relaxed);
                }
                return Ok(true);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(SCAN_POLL).await;
        }
    }
    pub async fn list_aps(&self, wifi_path: &OwnedObjectPath) -> Result<Vec<Ap>> {
        let wifi = WirelessProxy::new_from_path(wifi_path.clone(), self.system_conn()).await?;
        let ap_paths = wifi.get_all_access_points().await.unwrap_or_default();
        if ap_paths.is_empty() {
            return Ok(Vec::new());
        }
        let n_paths = ap_paths.len();
        let rows = map_bounded(ap_paths, |path| {
            let conn = self.system.clone();
            async move { ap_props(&conn, &path).await }
        })
        .await;
        let completed = self.scan_completed.load(Ordering::Relaxed);
        let fresh = completed > 0
            && boottime_secs()
                .map(|now| now.saturating_sub(completed) <= STALE_HORIZON.as_secs())
                .unwrap_or(false);
        tracing::debug!(completed, fresh, "stale filter state");
        let (_vpn_index, saved) = self.profile_index().await;
        let priority_of = |ssid: &str| saved.get(ssid).copied().unwrap_or(0);
        let mut by_ssid: std::collections::HashMap<String, Ap> = std::collections::HashMap::new();
        for props in rows.into_iter().flatten() {
            let Some(ssid) = props.ssid else { continue };
            let tolerance = if self.popup_visible.load(Ordering::Relaxed) {
                STALE_TOLERANCE_VISIBLE
            } else {
                STALE_TOLERANCE_HIDDEN
            };
            if fresh && is_ghost(props.last_seen, completed, tolerance) {
                tracing::debug!(
                    ssid = %ssid,
                    last_seen = props.last_seen,
                    completed,
                    "pruning stale ap"
                );
                continue;
            }
            let is_saved = saved.contains_key(&ssid);
            match by_ssid.get_mut(&ssid) {
                Some(slot) => {
                    if props.strength > slot.strength {
                        slot.strength = props.strength;
                        slot.freq_mhz = props.freq_mhz;
                    }
                    slot.bands |= aps::band_bit(props.freq_mhz);
                    slot.secured = props.secured;
                    slot.enterprise = props.enterprise;
                    slot.wep = props.wep;
                    slot.saved = is_saved;
                    slot.known = is_saved && self.has_connected(&ssid).await;
                    slot.priority = priority_of(&ssid);
                }
                None => {
                    by_ssid.insert(
                        ssid.clone(),
                        Ap {
                            saved: is_saved,
                            known: is_saved && self.has_connected(&ssid).await,
                            priority: priority_of(&ssid),
                            bands: aps::band_bit(props.freq_mhz),
                            ssid,
                            strength: props.strength,
                            freq_mhz: props.freq_mhz,
                            secured: props.secured,
                            enterprise: props.enterprise,
                            wep: props.wep,
                        },
                    );
                }
            }
        }
        let mut aps: Vec<Ap> = by_ssid.into_values().collect();
        tracing::debug!(paths = n_paths, ssids = aps.len(), "ap list built");
        crate::state::sort_aps(&mut aps);
        Ok(aps)
    }
    pub async fn autoconnect_candidates(&self) -> Vec<(String, i32)> {
        let (_, saved) = self.profile_index_fresh().await;
        let mut out: Vec<(String, i32)> = saved.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
    }
    pub async fn drop_profile_cache(&self) {
        *self.profiles.lock().await = None;
    }
    pub async fn profile_index(&self) -> ProfileIndex {
        {
            let cached = self.profiles.lock().await;
            if let Some((at, data)) = cached.as_ref()
                && at.elapsed() < PROFILE_TTL
            {
                return data.as_ref().clone();
            }
        }
        let fresh = self.profile_index_uncached().await;
        *self.profiles.lock().await = Some((Instant::now(), Arc::new(fresh.clone())));
        fresh
    }
    pub async fn profile_index_fresh(&self) -> ProfileIndex {
        let fresh = self.profile_index_uncached().await;
        *self.profiles.lock().await = Some((Instant::now(), Arc::new(fresh.clone())));
        fresh
    }
    async fn profile_index_uncached(&self) -> ProfileIndex {
        let Ok(settings) = rusty_network_manager::SettingsProxy::new(self.system_conn()).await
        else {
            return (Vec::new(), Default::default());
        };
        let Ok(paths) = settings.list_connections().await else {
            return (Vec::new(), Default::default());
        };
        let details = map_bounded(paths, |path| {
            let conn = self.system.clone();
            async move {
                let map = get_settings(&conn, &path).await;
                (path, map)
            }
        })
        .await;
        let active = self.active_vpn_ids().await;
        let mut vpns = Vec::new();
        let mut saved = std::collections::HashMap::new();
        for (path, map) in details {
            let Some(map) = map else { continue };
            let kind = map
                .get("connection")
                .and_then(|c| c.get("type"))
                .and_then(|v| match &**v {
                    Value::Str(s) => Some(s.to_string()),
                    _ => None,
                });
            match kind.as_deref() {
                Some("vpn") | Some("wireguard") => {
                    let id = map
                        .get("connection")
                        .and_then(|c| c.get("id"))
                        .and_then(|v| match &**v {
                            Value::Str(s) => Some(s.to_string()),
                            _ => None,
                        })
                        .unwrap_or_else(|| path.to_string());
                    let is_active = active.contains(&id);
                    vpns.push(VpnConnection {
                        id,
                        active: is_active,
                        path: path.to_string(),
                    });
                }
                Some("802-11-wireless") => {
                    if profiles::is_hotspot_profile(&map) {
                        continue;
                    }
                    if profile_unsaved(&self.system, &path).await {
                        continue;
                    }
                    if let Some(ssid) = profiles::wireless_ssid(&map) {
                        saved.insert(ssid, profiles::autoconnect_priority(&map));
                    }
                }
                _ => {}
            }
        }
        (vpns, saved)
    }
    pub async fn active_vpn_ids(&self) -> std::collections::HashSet<String> {
        let Ok(actives) = self.nm.active_connections().await else {
            return Default::default();
        };
        map_bounded(actives, |path| {
            let conn = self.system.clone();
            async move {
                let Ok(proxy) = ActiveProxy::new_from_path(path, &conn).await else {
                    return None;
                };
                if !matches!(proxy.type_().await.as_deref(), Ok("vpn") | Ok("wireguard")) {
                    return None;
                }
                proxy.id().await.ok()
            }
        })
        .await
        .into_iter()
        .flatten()
        .collect()
    }
    pub async fn active_ssid(&self) -> Result<Option<String>> {
        let actives = self.nm.active_connections().await.unwrap_or_default();
        let ids = map_bounded(actives, |path| {
            let conn = self.system.clone();
            async move {
                let Ok(proxy) = ActiveProxy::new_from_path(path, &conn).await else {
                    return None;
                };
                if proxy.state().await.unwrap_or(0) != 2 {
                    return None;
                }
                if proxy.type_().await.as_deref() != Ok("802-11-wireless") {
                    return None;
                }
                proxy.id().await.ok()
            }
        })
        .await;
        Ok(ids.into_iter().flatten().next())
    }
    pub async fn activating_ssid(&self) -> Result<Option<String>> {
        let actives = self.nm.active_connections().await.unwrap_or_default();
        let ids = map_bounded(actives, |path| {
            let conn = self.system.clone();
            async move {
                let Ok(proxy) = ActiveProxy::new_from_path(path, &conn).await else {
                    return None;
                };
                if proxy.state().await.unwrap_or(0) != 1 {
                    return None;
                }
                if proxy.type_().await.as_deref() != Ok("802-11-wireless") {
                    return None;
                }
                proxy.id().await.ok()
            }
        })
        .await;
        Ok(ids.into_iter().flatten().next())
    }
    pub async fn vpn_connections(&self) -> Result<Vec<VpnConnection>> {
        Ok(self.profile_index().await.0)
    }
    pub async fn refresh_model(&self) -> Result<Model> {
        let (wifi_enabled, networking_enabled) =
            futures::try_join!(self.nm.wireless_enabled(), self.nm.networking_enabled())
                .unwrap_or((true, true));
        let (connectivity, connectivity_check, primary_type) = futures::try_join!(
            self.nm.connectivity(),
            self.nm.connectivity_check_enabled(),
            self.nm.primary_connection_type()
        )
        .unwrap_or((0, false, String::new()));
        let wifi_path = self.wifi_device_path().await?;
        let mut aps = Vec::new();
        if let Some(ref path) = wifi_path {
            aps = self.list_aps(path).await.unwrap_or_default();
        }
        let mut active_ssid = self.active_ssid().await.unwrap_or(None);
        let activating_ssid = self.activating_ssid().await.unwrap_or(None);
        let vpn_connections = self.vpn_connections().await.unwrap_or_default();
        let (_, saved_profiles) = self.profile_index().await;
        let mut saved_ssids: Vec<String> = saved_profiles.into_keys().collect();
        saved_ssids.sort();
        tracing::debug!(count = saved_ssids.len(), ?saved_ssids, "saved profiles");
        let net = details::active_details(self, wifi_path.clone()).await;
        let wired = details::wired_status(self).await;
        let hotspot = details::hotspot_status(self).await.unwrap_or(None);
        if let Some(hs) = hotspot.as_ref().filter(|h| h.active) {
            aps.retain(|ap| ap.ssid != hs.ssid);
            if active_ssid.as_deref() == Some(hs.ssid.as_str()) {
                active_ssid = None;
            }
        }
        Ok(Model {
            aps: aps.into(),
            active_ssid,
            activating_ssid,
            active_iface: net.iface,
            active_ipv4: net.ipv4,
            active_bitrate_kbps: net.bitrate_kbps,
            active_gateway: net.gateway,
            active_dns: net.dns,
            active_freq_mhz: net.freq_mhz,
            saved_ssids: saved_ssids.clone().into(),
            wired,
            hotspot,
            wifi_enabled,
            networking_enabled,
            nm_online: true,
            no_internet: {
                let raw_bad = connectivity_check && matches!(connectivity, 1..=3);
                let mut guard = self.conn_bad_since.lock().await;
                let now = Instant::now();
                if raw_bad {
                    let since = *guard.get_or_insert(now);
                    now.duration_since(since) >= CONN_BAD_GRACE
                } else {
                    *guard = None;
                    false
                }
            },
            primary_wired: primary_type == "802-3-ethernet",
            vpn_connections: vpn_connections.into(),
        })
    }
}
struct ApProps {
    ssid: Option<String>,
    strength: u8,
    freq_mhz: Option<u32>,
    secured: bool,
    enterprise: bool,
    wep: bool,
    last_seen: u64,
}
async fn ap_props(conn: &Connection, path: &OwnedObjectPath) -> Option<ApProps> {
    use zbus::fdo::PropertiesProxy;
    let iface = zbus::names::InterfaceName::try_from(AP_INTERFACE).ok()?;
    let proxy = PropertiesProxy::builder(conn)
        .destination("org.freedesktop.NetworkManager")
        .ok()?
        .path(path.clone())
        .ok()?
        .build()
        .await
        .ok()?;
    let all = proxy.get_all(iface).await.ok()?;
    let u8_of = |k: &str| -> u8 { all.get(k).and_then(|v| u8::try_from(v).ok()).unwrap_or(0) };
    let u32_of = |k: &str| -> u32 { all.get(k).and_then(|v| u32::try_from(v).ok()).unwrap_or(0) };
    let flags = u32_of("Flags");
    let wpa = u32_of("WpaFlags");
    let rsn = u32_of("RsnFlags");
    let ssid = all
        .get("Ssid")
        .and_then(bytes_to_ssid)
        .filter(|s| !s.is_empty());
    let last_seen = match all.get("LastSeen").map(|v| &**v) {
        Some(Value::I32(n)) => u64::try_from(*n).unwrap_or(0),
        Some(Value::U32(n)) => (*n).into(),
        Some(Value::I64(n)) => u64::try_from(*n).unwrap_or(0),
        Some(Value::U64(n)) => *n,
        _ => 0,
    };
    Some(ApProps {
        ssid,
        strength: u8_of("Strength"),
        freq_mhz: {
            let f = u32_of("Frequency");
            if f > 0 { Some(f) } else { None }
        },
        secured: aps::is_secured(wpa, rsn) || aps::is_wep_only(flags, wpa, rsn),
        enterprise: aps::is_enterprise(wpa, rsn),
        wep: aps::is_wep_only(flags, wpa, rsn),
        last_seen,
    })
}
pub(crate) fn bytes_to_ssid(v: &OwnedValue) -> Option<String> {
    match &**v {
        Value::Array(arr) => {
            let bytes: Vec<u8> = arr.iter().filter_map(|b| u8::try_from(b).ok()).collect();
            aps::decode_ssid(&bytes)
        }
        _ => None,
    }
}
pub(crate) async fn get_settings(
    conn: &Connection,
    path: &OwnedObjectPath,
) -> Option<std::collections::HashMap<String, std::collections::HashMap<String, OwnedValue>>> {
    use rusty_network_manager::SettingsConnectionProxy;
    let proxy = SettingsConnectionProxy::new_from_path(path.clone(), conn)
        .await
        .ok()?;
    proxy.get_settings().await.ok()
}
pub(crate) async fn profile_unsaved(conn: &Connection, path: &OwnedObjectPath) -> bool {
    use zbus::fdo::PropertiesProxy;
    use zbus::names::InterfaceName;
    let props = PropertiesProxy::builder(conn)
        .destination("org.freedesktop.NetworkManager")
        .and_then(|b| b.path(path.clone()));
    let Ok(props) = props else {
        return false;
    };
    let Ok(props) = props.build().await else {
        return false;
    };
    let Ok(iface) = InterfaceName::try_from("org.freedesktop.NetworkManager.Settings.Connection")
    else {
        return false;
    };
    let Ok(value) = props.get(iface, "Unsaved").await else {
        return false;
    };
    matches!(&*value, Value::Bool(true))
}
pub(crate) async fn saved_profile_path(
    conn: &Connection,
    ssid: &str,
) -> Result<Option<OwnedObjectPath>> {
    profiles::saved_profile_path(conn, ssid).await
}
pub(crate) async fn active_hotspot_profile(
    conn: &Connection,
) -> Result<
    Option<(
        OwnedObjectPath,
        std::collections::HashMap<String, std::collections::HashMap<String, OwnedValue>>,
    )>,
> {
    use rusty_network_manager::{ActiveProxy, NetworkManagerProxy};
    let nm = NetworkManagerProxy::new(conn).await?;
    let actives = nm.active_connections().await.unwrap_or_default();
    let wifi = map_bounded(actives, |path| {
        let conn = conn.clone();
        async move {
            let Ok(proxy) = ActiveProxy::new_from_path(path.clone(), &conn).await else {
                return None;
            };
            if proxy.type_().await.as_deref() != Ok("802-11-wireless") {
                return None;
            }
            if proxy.state().await.unwrap_or(0) != 2 {
                return None;
            }
            let conn_path = proxy.connection().await.ok()?;
            let map = get_settings(&conn, &conn_path).await?;
            Some((conn_path, map))
        }
    })
    .await;
    for (path, map) in wifi.into_iter().flatten() {
        let is_ap = map
            .get("802-11-wireless")
            .and_then(|w| w.get("mode"))
            .map(|v| matches!(&**v, Value::Str(s) if s.as_str() == "ap"))
            .unwrap_or(false);
        if is_ap {
            return Ok(Some((path, map)));
        }
    }
    Ok(None)
}
pub(crate) async fn active_conn_is_hotspot(conn: &Connection, path: &OwnedObjectPath) -> bool {
    use rusty_network_manager::ActiveProxy;
    let Ok(proxy) = ActiveProxy::new_from_path(path.clone(), conn).await else {
        return false;
    };
    if proxy.type_().await.as_deref() != Ok("802-11-wireless") {
        return false;
    }
    let Ok(conn_path) = proxy.connection().await else {
        return false;
    };
    let Some(map) = get_settings(conn, &conn_path).await else {
        return false;
    };
    map.get("802-11-wireless")
        .and_then(|w| w.get("mode"))
        .map(|v| matches!(&**v, Value::Str(s) if s.as_str() == "ap"))
        .unwrap_or(false)
}

#[cfg(test)]
mod ghost_tests {
    use super::*;

    #[test]
    fn ghost_is_dropped_after_tolerance() {
        assert!(is_ghost(1_000, 1_200, 60));
        assert!(is_ghost(1_100, 1_200, 60));
    }

    #[test]
    fn fresh_ap_is_kept() {
        assert!(!is_ghost(1_190, 1_200, 60));
        assert!(!is_ghost(1_200, 1_200, 60));
    }

    #[test]
    fn unknown_or_unscanned_is_kept() {
        assert!(!is_ghost(0, 1_200, 60));
        assert!(!is_ghost(500, 0, 60));
    }

    #[test]
    fn boottime_is_plausible() {
        let secs = boottime_secs().expect("boottime");
        assert!(secs < 100_000_000, "uptime should stay well below epoch");
    }
}
