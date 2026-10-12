pub mod aps;
pub mod connect;
pub mod details;
pub mod hotspot;
pub mod known;
pub mod profiles;
pub mod radio;
pub mod signals;
use self::aps::{Bands, Security};
use crate::state::{Ap, Model, Ssid, VpnConnection};
use anyhow::{Context, Result};
use rusty_network_manager::{ActiveProxy, DeviceProxy, NetworkManagerProxy, WirelessProxy};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
pub const NM_DEVICE_TYPE_WIFI: u32 = 2;
const FANOUT: usize = 24;
const SCAN_POLL: Duration = Duration::from_millis(500);
const PROFILE_TTL: Duration = Duration::from_secs(30);
const CONN_BAD_GRACE: Duration = Duration::from_secs(5);
pub const SCAN_WAIT: Duration = Duration::from_secs(12);
const STALE_HORIZON: Duration = Duration::from_secs(120);

pub const DBUS_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

const STALE_TOLERANCE_SECS: u64 = 60;
const NM_SCAN_MS_PER_SEC: u64 = 1000;
const STALE_MISS_LIMIT: u32 = 3;

pub fn boottime_secs() -> Option<u64> {
    let uptime = std::fs::read_to_string("/proc/uptime").ok()?;
    let secs = uptime.split_whitespace().next()?;
    secs.split('.').next()?.parse().ok()
}

pub fn is_ghost(last_seen: u64, scan_completed: u64, tolerance: u64) -> bool {
    last_seen > 0 && scan_completed > 0 && last_seen.saturating_add(tolerance) < scan_completed
}

fn prune_ghost(ghost: bool, fresh: bool, misses: u32) -> bool {
    ghost && fresh && misses >= STALE_MISS_LIMIT
}
const AP_INTERFACE: &str = "org.freedesktop.NetworkManager.AccessPoint";
macro_rules! uncached_proxy {
    ($proxy:ty, $conn:expr, $path:expr) => {{
        match <$proxy>::builder($conn).path($path) {
            Ok(builder) => {
                match builder
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .build()
                    .await
                {
                    Ok(proxy) => Some(proxy),
                    Err(_) => None,
                }
            }
            Err(_) => None,
        }
    }};
}
pub(crate) use uncached_proxy;
pub async fn map_bounded<I, T, F, Fut, R>(items: I, f: F) -> Vec<R>
where
    I: IntoIterator<Item = T>,
    F: Fn(T) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = R> + Send + 'static,
    T: Send + 'static,
    R: Send + 'static,
{
    let mut out = Vec::new();
    let mut batch: Vec<T> = Vec::with_capacity(FANOUT);
    for item in items {
        batch.push(item);
        if batch.len() < FANOUT {
            continue;
        }
        drain_batch(&mut batch, &f, &mut out).await;
    }
    if !batch.is_empty() {
        drain_batch(&mut batch, &f, &mut out).await;
    }
    out
}
async fn drain_batch<T, F, Fut, R>(batch: &mut Vec<T>, f: &F, out: &mut Vec<R>)
where
    F: Fn(T) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = R> + Send + 'static,
    T: Send + 'static,
    R: Send + 'static,
{
    let mut set = tokio::task::JoinSet::new();
    for item in batch.drain(..) {
        let task = f.clone();
        set.spawn(async move { task(item).await });
    }
    while let Some(done) = set.join_next().await {
        match done {
            Ok(value) => out.push(value),
            Err(e) => tracing::warn!("bounded task failed: {e}"),
        }
    }
}
pub type ProfileIndex = (
    Vec<VpnConnection>,
    std::collections::HashMap<String, i32>,
    std::collections::HashMap<String, u64>,
);

pub struct NmClient {
    system: Connection,
    pub nm: NetworkManagerProxy<'static>,
    profiles: tokio::sync::Mutex<Option<(Instant, Arc<ProfileIndex>)>>,
    scan_completed: AtomicU64,
    connected_once: tokio::sync::RwLock<std::collections::BTreeSet<String>>,
    popup_visible: std::sync::Arc<std::sync::atomic::AtomicBool>,
    conn_bad_since: tokio::sync::Mutex<Option<Instant>>,
    stale_misses: tokio::sync::Mutex<std::collections::HashMap<String, u32>>,
    scan_inflight: Arc<AtomicBool>,
    hotspot_expected_ssid: tokio::sync::Mutex<Option<String>>,
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
            stale_misses: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            scan_inflight: Arc::new(AtomicBool::new(false)),
            hotspot_expected_ssid: tokio::sync::Mutex::new(None),
        })
    }
    pub fn system_conn(&self) -> &Connection {
        &self.system
    }
    pub fn popup_visible(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.popup_visible.clone()
    }
    pub fn scan_inflight(&self) -> bool {
        self.scan_inflight.load(Ordering::Relaxed)
    }
    pub fn scan_completed_recently(&self, within_secs: u64) -> bool {
        let completed = self.scan_completed.load(Ordering::Relaxed);
        boottime_secs()
            .map(|now| completed > 0 && now.saturating_sub(completed) <= within_secs)
            .unwrap_or(false)
    }
    pub async fn set_hotspot_expected_ssid(&self, ssid: Option<String>) {
        *self.hotspot_expected_ssid.lock().await = ssid;
    }
    pub async fn hotspot_expected(&self) -> Option<String> {
        self.hotspot_expected_ssid.lock().await.clone()
    }
    pub async fn mark_connected(&self, ssid: &str) {
        let snapshot = {
            let mut set = self.connected_once.write().await;
            if !set.insert(ssid.to_string()) {
                return;
            }
            set.clone()
        };
        let _ = tokio::task::spawn_blocking(move || known::save(&snapshot))
            .await
            .inspect_err(|e| tracing::warn!("connected-list save task panicked: {e:#}"));
        tracing::info!(ssid, "network marked as previously connected");
    }
    pub async fn version(&self) -> Result<String> {
        Ok(self.nm.version().await?)
    }
    pub async fn wifi_device_path(&self) -> Result<Option<OwnedObjectPath>> {
        let devices = self.nm.get_devices().await?;
        let conn = self.system.clone();
        let types = map_bounded(devices, move |path| {
            let conn = conn.clone();
            async move {
                let dev = uncached_proxy!(DeviceProxy, &conn, path.clone())?;
                Some((path, dev.device_type().await.unwrap_or(0)))
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
        struct ScanGuard<'a> {
            flag: &'a Arc<AtomicBool>,
        }
        impl Drop for ScanGuard<'_> {
            fn drop(&mut self) {
                self.flag.store(false, Ordering::Relaxed);
            }
        }
        self.scan_inflight.store(true, Ordering::Relaxed);
        let _guard = ScanGuard {
            flag: &self.scan_inflight,
        };
        let wifi = WirelessProxy::new_from_path(wifi_path.clone(), self.system_conn()).await?;
        let before = wifi.last_scan().await.unwrap_or(0);
        if let Err(e) = wifi.request_scan(std::collections::HashMap::new()).await {
            tracing::warn!("NM rejected scan request: {e:#}");
        }
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
        let Some(wifi) = uncached_proxy!(WirelessProxy, self.system_conn(), wifi_path.clone())
        else {
            anyhow::bail!("could not open the Wi-Fi device");
        };
        if let Ok(stamp) = wifi.last_scan().await
            && stamp > 0
        {
            self.scan_completed.fetch_max(
                stamp as u64 / NM_SCAN_MS_PER_SEC,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        let ap_paths = wifi.get_all_access_points().await.unwrap_or_default();
        if ap_paths.is_empty() {
            return Ok(Vec::new());
        }
        let n_paths = ap_paths.len();
        let conn = self.system.clone();
        let rows = map_bounded(ap_paths, move |path| {
            let conn = conn.clone();
            async move { ap_props(&conn, &path).await }
        })
        .await;
        let completed = self.scan_completed.load(Ordering::Relaxed);
        let fresh = completed > 0
            && boottime_secs()
                .map(|now| now.saturating_sub(completed) <= STALE_HORIZON.as_secs())
                .unwrap_or(false);
        tracing::debug!(completed, fresh, "stale filter state");
        let index = self.profile_index().await;
        let saved = &index.1;
        let priority_of = |ssid: &str| saved.get(ssid).copied().unwrap_or(0);
        let connected = self.connected_once.read().await.clone();
        if !fresh {
            self.stale_misses.lock().await.clear();
        }
        let mut by_ssid: std::collections::HashMap<String, Ap> = std::collections::HashMap::new();
        for props in rows.into_iter().flatten() {
            let Some(ssid) = props.ssid else { continue };
            let ghost = fresh && is_ghost(props.last_seen, completed, STALE_TOLERANCE_SECS);
            if ghost {
                let mut misses = self.stale_misses.lock().await;
                let count = misses.entry(ssid.clone()).or_default();
                *count += 1;
                if prune_ghost(ghost, fresh, *count) {
                    tracing::debug!(
                        ssid = %ssid,
                        last_seen = props.last_seen,
                        completed,
                        misses = *count,
                        "pruning stale ap"
                    );
                    continue;
                }
            } else if fresh {
                self.stale_misses.lock().await.remove(&ssid);
            }
            let is_saved = saved.contains_key(&ssid);
            match by_ssid.get_mut(&ssid) {
                Some(slot) => {
                    if props.strength > slot.strength {
                        slot.strength = props.strength;
                        slot.freq_mhz = props.freq_mhz;
                    }
                    slot.bands |= Bands::from_freq(props.freq_mhz);
                    slot.security = slot.security.stricter(props.security);
                    slot.saved = is_saved;
                    slot.known = is_saved && connected.contains(&ssid);
                    slot.priority = priority_of(&ssid);
                }
                None => {
                    by_ssid.insert(
                        ssid.clone(),
                        Ap {
                            saved: is_saved,
                            known: is_saved && connected.contains(&ssid),
                            priority: priority_of(&ssid),
                            bands: Bands::from_freq(props.freq_mhz),
                            ssid: ssid.clone().into(),
                            strength: props.strength,
                            freq_mhz: props.freq_mhz,
                            security: props.security,
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
    pub async fn drop_profile_cache(&self) {
        *self.profiles.lock().await = None;
    }
    pub async fn profile_index(&self) -> Arc<ProfileIndex> {
        let mut cached = self.profiles.lock().await;
        if let Some((at, data)) = cached.as_ref()
            && at.elapsed() < PROFILE_TTL
        {
            return data.clone();
        }
        let fresh = self.profile_index_uncached().await;
        let fresh = Arc::new(fresh);
        *cached = Some((Instant::now(), fresh.clone()));
        fresh
    }
    async fn profile_index_uncached(&self) -> ProfileIndex {
        let Ok(settings) = rusty_network_manager::SettingsProxy::new(self.system_conn()).await
        else {
            return (Vec::new(), Default::default(), Default::default());
        };
        let Ok(paths) = settings.list_connections().await else {
            return (Vec::new(), Default::default(), Default::default());
        };
        let conn = self.system.clone();
        let details = map_bounded(paths, move |path| {
            let conn = conn.clone();
            async move {
                let (map, unsaved) =
                    tokio::join!(get_settings(&conn, &path), profile_unsaved(&conn, &path));
                (path, map, unsaved)
            }
        })
        .await;
        let active = self.active_vpn_ids().await;
        let mut vpns = Vec::new();
        let mut saved = std::collections::HashMap::new();
        let mut timestamps = std::collections::HashMap::new();
        for (path, map, unsaved) in details {
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
                    if unsaved {
                        continue;
                    }
                    if let Some(ssid) = profiles::wireless_ssid(&map) {
                        saved.insert(ssid.clone(), profiles::autoconnect_priority(&map));
                        if let Some(ts) = profiles::connection_timestamp(&map) {
                            timestamps.insert(ssid, ts);
                        }
                    }
                }
                _ => {}
            }
        }
        (vpns, saved, timestamps)
    }
    pub async fn active_vpn_ids(&self) -> std::collections::HashSet<String> {
        let Ok(actives) = self.nm.active_connections().await else {
            return Default::default();
        };
        let conn = self.system.clone();
        map_bounded(actives, move |path| {
            let conn = conn.clone();
            async move {
                let builder = ActiveProxy::builder(&conn);
                let Ok(builder) = builder.path(path) else {
                    return None;
                };
                let Ok(proxy) = builder
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .build()
                    .await
                else {
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
    pub async fn active_ssid(&self) -> Result<Option<Ssid>> {
        Ok(self
            .active_wifi_snapshot()
            .await
            .map(|(_, ssid)| ssid.into()))
    }
    pub async fn activating_ssid(&self) -> Result<Option<Ssid>> {
        let actives = self.nm.active_connections().await.unwrap_or_default();
        let conn = self.system.clone();
        let rows = map_bounded(actives, move |path| {
            let conn = conn.clone();
            async move {
                let proxy = uncached_proxy!(ActiveProxy, &conn, path)?;
                if proxy.state().await.unwrap_or(0) != 1 {
                    return None;
                }
                if proxy.type_().await.as_deref() != Ok("802-11-wireless") {
                    return None;
                }
                let Ok(profile) = proxy.connection().await else {
                    return None;
                };
                let map = get_settings(&conn, &profile).await?;
                profiles::wireless_ssid(&map)
            }
        })
        .await;
        Ok(rows.into_iter().flatten().next().map(Ssid::from))
    }
    pub async fn vpn_connections(&self) -> Result<Vec<VpnConnection>> {
        Ok(self.profile_index().await.0.clone())
    }
    pub async fn refresh_model(&self) -> Result<Model> {
        let (wifi_enabled, networking_enabled) =
            match tokio::try_join!(self.nm.wireless_enabled(), self.nm.networking_enabled()) {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!("NM radio state query failed: {e:#}");
                    (true, true)
                }
            };
        let (connectivity, connectivity_check, primary_type) = match tokio::try_join!(
            self.nm.connectivity(),
            self.nm.connectivity_check_enabled(),
            self.nm.primary_connection_type()
        ) {
            Ok(triple) => triple,
            Err(e) => {
                tracing::warn!("NM connectivity query failed: {e:#}");
                (0, false, String::new())
            }
        };
        let wifi_path = self.wifi_device_path().await?;
        let mut aps = Vec::new();
        if let Some(ref path) = wifi_path {
            aps = self.list_aps(path).await.unwrap_or_default();
        }
        let mut active_ssid = self.active_ssid().await.unwrap_or(None);
        let activating_ssid = self.activating_ssid().await.unwrap_or(None);
        let vpn_connections = self.vpn_connections().await.unwrap_or_default();
        let index = self.profile_index().await;
        let saved_profiles = &index.1;
        let timestamps = &index.2;
        let mut saved_ssids: Vec<Ssid> = saved_profiles
            .keys()
            .map(|s| Ssid::from(s.as_str()))
            .collect();
        saved_ssids.sort();
        tracing::debug!(count = saved_ssids.len(), "saved profiles");
        let visible_ssids: std::collections::HashSet<Ssid> =
            aps.iter().map(|ap| ap.ssid.clone()).collect();
        let last_good = saved_ssids
            .iter()
            .filter(|ssid| visible_ssids.contains(*ssid))
            .max_by_key(|ssid| timestamps.get(ssid.as_str()).copied().unwrap_or(0))
            .cloned();
        let net = details::active_details(self, wifi_path.clone()).await;
        let wired = details::wired_status(self).await;
        let hotspot = details::hotspot_status(self).await.unwrap_or(None);
        let hs_active = hotspot.as_ref().map(|h| h.active).unwrap_or(false);
        if let Some(hs) = hotspot.as_ref().filter(|h| h.active) {
            aps.retain(|ap| ap.ssid != hs.ssid);
            if active_ssid.as_deref() == Some(hs.ssid.as_str()) {
                active_ssid = None;
            }
        }
        if let Some(expected) = self.hotspot_expected_ssid.lock().await.clone() {
            if !hs_active {
                tracing::info!(expected, "hotspot expected but not detected");
            }
            aps.retain(|ap| ap.ssid.as_str() != expected.as_str());
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
            last_good,
            scanning: self.scan_inflight.load(Ordering::Relaxed),
        })
    }
}
struct ApProps {
    ssid: Option<String>,
    strength: u8,
    freq_mhz: Option<u32>,
    security: Security,
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
        .cache_properties(zbus::proxy::CacheProperties::No)
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
        security: aps::classify_security(wpa, rsn, flags),
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
    let proxy = uncached_proxy!(SettingsConnectionProxy, conn, path.clone())?;
    proxy.get_settings().await.ok()
}
pub(crate) async fn profile_unsaved(conn: &Connection, path: &OwnedObjectPath) -> bool {
    use zbus::fdo::PropertiesProxy;
    use zbus::names::InterfaceName;
    let Ok(builder) = PropertiesProxy::builder(conn)
        .destination("org.freedesktop.NetworkManager")
        .and_then(|b| b.path(path.clone()))
    else {
        return false;
    };
    let Ok(props) = builder
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .await
    else {
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
    use rusty_network_manager::NetworkManagerProxy;
    let nm = NetworkManagerProxy::new(conn).await?;
    let actives = nm.active_connections().await.unwrap_or_default();
    let conn: Connection = (*conn).clone();
    let wifi = map_bounded(actives, move |path| {
        let conn = conn.clone();
        async move {
            use zbus::fdo::PropertiesProxy;
            let iface = zbus::names::InterfaceName::try_from(
                "org.freedesktop.NetworkManager.Connection.Active",
            )
            .ok()?;
            let proxy = PropertiesProxy::builder(&conn)
                .destination("org.freedesktop.NetworkManager")
                .ok()?
                .path(path)
                .ok()?
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await
                .ok()?;
            let all = proxy.get_all(iface).await.ok()?;
            let str_of = |k: &str| -> Option<String> {
                all.get(k).and_then(|v| match &**v {
                    Value::Str(s) => Some(s.to_string()),
                    _ => None,
                })
            };
            if str_of("Type").as_deref() != Some("802-11-wireless") {
                return None;
            }
            let state = all
                .get("State")
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(0);
            if state != 2 {
                return None;
            }
            let id_match = str_of("Id")
                .map(|id| id == hotspot::HOTSPOT_CONN_ID || id == hotspot::HOTSPOT_SSID)
                .unwrap_or(false);
            let conn_path: OwnedObjectPath = all.get("Connection").and_then(|v| match &**v {
                Value::ObjectPath(p) => Some(p.to_owned().into()),
                _ => None,
            })?;
            if id_match {
                return Some((conn_path, None, true));
            }
            let map = get_settings(&conn, &conn_path).await;
            Some((conn_path, map, false))
        }
    })
    .await;
    for (path, map, id_match) in wifi.into_iter().flatten() {
        if let Some(map) = map.as_ref() {
            let is_ap = map
                .get("802-11-wireless")
                .and_then(|w| w.get("mode"))
                .map(|v| matches!(&**v, Value::Str(s) if s.as_str() == "ap"))
                .unwrap_or(false);
            if is_ap {
                return Ok(Some((path, map.clone())));
            }
        }
        if id_match {
            let mut wireless = std::collections::HashMap::new();
            wireless.insert(
                "mode".to_string(),
                OwnedValue::try_from(Value::new("ap")).expect("static str encodes"),
            );
            let mut map = std::collections::HashMap::new();
            map.insert("802-11-wireless".to_string(), wireless);
            return Ok(Some((path, map)));
        }
    }
    Ok(None)
}
pub(crate) async fn active_conn_is_hotspot(conn: &Connection, path: &OwnedObjectPath) -> bool {
    use rusty_network_manager::ActiveProxy;
    let Some(proxy) = uncached_proxy!(ActiveProxy, conn, path.clone()) else {
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
    fn prune_requires_two_consecutive_misses_in_fresh_window() {
        assert!(!prune_ghost(true, true, 0));
        assert!(!prune_ghost(true, true, STALE_MISS_LIMIT - 1));
        assert!(prune_ghost(true, true, STALE_MISS_LIMIT));
        assert!(prune_ghost(true, true, STALE_MISS_LIMIT + 1));
    }

    #[test]
    fn prune_never_inside_stale_or_present_window() {
        assert!(!prune_ghost(true, false, 99));
        assert!(!prune_ghost(false, true, 99));
        assert!(!prune_ghost(false, false, 99));
    }

    #[test]
    fn boottime_is_plausible() {
        let secs = boottime_secs().expect("boottime");
        assert!(secs < 100_000_000, "uptime should stay well below epoch");
    }
}
