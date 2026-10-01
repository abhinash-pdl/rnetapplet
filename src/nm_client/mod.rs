pub mod aps;
pub mod connect;
pub mod details;
pub mod hotspot;
pub mod profiles;
pub mod radio;
pub mod signals;
use crate::state::{Ap, Model, VpnConnection};
use anyhow::{Context, Result};
use futures::StreamExt as _;
use rusty_network_manager::{ActiveProxy, DeviceProxy, NetworkManagerProxy, WirelessProxy};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
pub const NM_DEVICE_TYPE_WIFI: u32 = 2;
const FANOUT: usize = 24;
const SCAN_POLL: Duration = Duration::from_millis(250);
const PROFILE_TTL: Duration = Duration::from_secs(30);
pub const SCAN_WAIT: Duration = Duration::from_secs(12);
const STALE_HORIZON: Duration = Duration::from_secs(120);

const STALE_TOLERANCE: u64 = 60;
const AP_INTERFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
const CONNECTION_INTERFACE: &str = "org.freedesktop.NetworkManager.Settings.Connection";
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
    scan_cutoff: AtomicU64,
    scan_completed: AtomicU64,
}
impl NmClient {
    pub async fn connect() -> Result<Self> {
        let system = Connection::system()
            .await
            .context("failed to connect to system D-Bus")?;
        let nm = NetworkManagerProxy::new(&system)
            .await
            .context("failed to create NetworkManager proxy")?;
        Ok(Self {
            system,
            nm,
            profiles: tokio::sync::Mutex::new(None),
            scan_cutoff: AtomicU64::new(0),
            scan_completed: AtomicU64::new(0),
        })
    }
    pub fn system_conn(&self) -> &Connection {
        &self.system
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
                let completed_at = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                self.scan_cutoff
                    .store((now / 1000) as u64, Ordering::Relaxed);
                self.scan_completed.store(completed_at, Ordering::Relaxed);
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
            self.scan_cutoff.store(0, Ordering::Relaxed);
            return Ok(Vec::new());
        }
        let n_paths = ap_paths.len();
        let rows = map_bounded(ap_paths, |path| {
            let conn = self.system.clone();
            async move { ap_props(&conn, &path).await }
        })
        .await;
        let cutoff = self.scan_cutoff.load(Ordering::Relaxed);
        let completed = self.scan_completed.load(Ordering::Relaxed);
        let fresh = completed > 0
            && cutoff > 0
            && SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs().saturating_sub(completed) <= STALE_HORIZON.as_secs())
                .unwrap_or(false);
        tracing::debug!(cutoff, fresh, "stale filter state");
        let (_vpn_index, saved) = self.profile_index().await;
        let priority_of = |ssid: &str| saved.get(ssid).copied().unwrap_or(0);
        let mut by_ssid: std::collections::HashMap<String, Ap> = std::collections::HashMap::new();
        for props in rows.into_iter().flatten() {
            let Some(ssid) = props.ssid else { continue };
            if fresh
                && props.last_seen > 0
                && props.last_seen.saturating_add(STALE_TOLERANCE) < cutoff
            {
                tracing::debug!(
                    ssid = %ssid,
                    last_seen = props.last_seen,
                    cutoff,
                    "pruning stale ap"
                );
                continue;
            }
            let is_saved = saved.contains_key(&ssid);
            match by_ssid.get_mut(&ssid) {
                Some(slot) if props.strength > slot.strength => {
                    slot.strength = props.strength;
                }
                Some(_) => {}
                None => {
                    by_ssid.insert(
                        ssid.clone(),
                        Ap {
                            saved: is_saved,
                            priority: priority_of(&ssid),
                            ssid,
                            strength: props.strength,
                            secured: props.secured,
                        },
                    );
                }
            }
        }
        let mut aps: Vec<Ap> = by_ssid.into_values().collect();
        tracing::debug!(paths = n_paths, ssids = aps.len(), "ap list built");
        aps.sort_by(|a, b| {
            b.saved
                .cmp(&a.saved)
                .then_with(|| b.priority.cmp(&a.priority))
                .then_with(|| b.strength.cmp(&a.strength))
                .then_with(|| a.ssid.cmp(&b.ssid))
        });
        Ok(aps)
    }
    pub async fn autoconnect_candidates(&self) -> Vec<(String, i32)> {
        let (_, saved) = self.profile_index_fresh().await;
        let mut out: Vec<(String, i32)> = saved.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        out
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
        let typed = map_bounded(paths, |path| {
            let conn = self.system.clone();
            async move {
                let ty = prop_str(&conn, &path, "Type").await;
                (path, ty)
            }
        })
        .await;
        let wanted: Vec<OwnedObjectPath> = typed
            .iter()
            .filter(|(_, t)| t.as_deref() == Some("802-11-wireless") || t.as_deref() == Some("vpn"))
            .map(|(p, _)| p.clone())
            .collect();
        let details = map_bounded(wanted, |path| {
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
                Some("vpn") => {
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
                    });
                }
                Some("802-11-wireless") => {
                    if !profiles::ever_connected(&map) {
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
                if proxy.type_().await.as_deref() != Ok("vpn") {
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
    pub async fn vpn_connections(&self) -> Result<Vec<VpnConnection>> {
        Ok(self.profile_index().await.0)
    }
    pub async fn refresh_model(&self) -> Result<Model> {
        let (wifi_enabled, networking_enabled) =
            futures::try_join!(self.nm.wireless_enabled(), self.nm.networking_enabled())
                .unwrap_or((true, true));
        let wifi_path = self.wifi_device_path().await?;
        let mut aps = Vec::new();
        if let Some(ref path) = wifi_path {
            aps = self.list_aps(path).await.unwrap_or_default();
        }
        let mut active_ssid = self.active_ssid().await.unwrap_or(None);
        let vpn_connections = self.vpn_connections().await.unwrap_or_default();
        let (active_iface, active_ipv4, active_bitrate_kbps) =
            details::active_details(self, wifi_path.clone()).await;
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
            active_iface,
            active_ipv4,
            active_bitrate_kbps,
            hotspot,
            wifi_enabled,
            networking_enabled,
            nm_online: true,
            vpn_connections: vpn_connections.into(),
        })
    }
}
struct ApProps {
    ssid: Option<String>,
    strength: u8,
    secured: bool,
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
        secured: aps::is_secured(u32_of("WpaFlags"), u32_of("RsnFlags")),
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
pub(crate) async fn prop_str(
    conn: &Connection,
    path: &OwnedObjectPath,
    name: &str,
) -> Option<String> {
    use zbus::fdo::PropertiesProxy;
    let iface = zbus::names::InterfaceName::try_from(CONNECTION_INTERFACE).ok()?;
    let proxy = PropertiesProxy::builder(conn)
        .destination("org.freedesktop.NetworkManager")
        .ok()?
        .path(path.clone())
        .ok()?
        .build()
        .await
        .ok()?;
    let value: OwnedValue = proxy.get(iface, name).await.ok()?;
    match value.into() {
        Value::Str(s) => Some(s.to_string()),
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
