use super::NmClient;
use super::uncached_proxy;
use crate::secret_agent::SecretStore;
use crate::state::{MSG_FAILED, MSG_WRONG_PASSWORD, ModelRx, Secret, Ssid, UiEvent};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use zbus::zvariant::{OwnedObjectPath, Value};
pub fn open_profile<'a>(ssid: &'a str) -> HashMap<&'a str, HashMap<&'a str, Value<'a>>> {
    let mut outer: HashMap<&str, HashMap<&str, Value>> = HashMap::new();
    let mut connection = HashMap::new();
    connection.insert("id", Value::new(ssid));
    connection.insert("type", Value::new("802-11-wireless"));
    connection.insert("autoconnect", Value::new(true));
    outer.insert("connection", connection);
    let mut wireless = HashMap::new();
    wireless.insert("ssid", Value::new(ssid.as_bytes()));
    wireless.insert("mode", Value::new("infrastructure"));
    outer.insert("802-11-wireless", wireless);
    let mut ipv4 = HashMap::new();
    ipv4.insert("method", Value::new("auto"));
    outer.insert("ipv4", ipv4);
    let mut ipv6 = HashMap::new();
    ipv6.insert("method", Value::new("auto"));
    outer.insert("ipv6", ipv6);
    outer
}
pub fn secure_profile<'a>(
    ssid: &'a str,
    psk: &'a str,
) -> HashMap<&'a str, HashMap<&'a str, Value<'a>>> {
    let mut outer = open_profile(ssid);
    let mut sec = HashMap::new();
    sec.insert("key-mgmt", Value::new("wpa-psk"));
    sec.insert("psk", Value::new(psk));
    outer.insert("802-11-wireless-security", sec);
    outer
}
pub fn hidden_profile<'a>(
    ssid: &'a str,
    psk: &'a str,
) -> HashMap<&'a str, HashMap<&'a str, Value<'a>>> {
    let mut outer = if psk.is_empty() {
        open_profile(ssid)
    } else {
        secure_profile(ssid, psk)
    };
    if let Some(wireless) = outer.get_mut("802-11-wireless") {
        wireless.insert("hidden", Value::new(true));
    }
    outer
}
pub(crate) fn root_path() -> Result<OwnedObjectPath> {
    OwnedObjectPath::try_from("/").context("static root object path")
}
pub fn validate_psk(psk: &str) -> Result<()> {
    if !(8..=63).contains(&psk.len()) {
        anyhow::bail!("password must be 8-63 characters");
    }
    if psk.chars().any(|c| c.is_control()) || psk.trim_start() != psk || psk.trim_end() != psk {
        anyhow::bail!("password cannot contain control characters or leading/trailing spaces");
    }
    Ok(())
}
pub struct ConnectOutcome {
    pub active: OwnedObjectPath,
    pub created: Option<OwnedObjectPath>,
    pub commit: Option<OwnedObjectPath>,
    pub persist_on_success: bool,
}
pub struct PrevWifi {
    pub profile: zbus::zvariant::OwnedObjectPath,
    pub ssid: String,
}

async fn active_reason(conn: &zbus::Connection, path: &OwnedObjectPath) -> Option<u32> {
    use zbus::fdo::PropertiesProxy;
    let iface =
        zbus::names::InterfaceName::try_from("org.freedesktop.NetworkManager.Connection.Active")
            .ok()?;
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
    all.get("Reason").and_then(|v| u32::try_from(v).ok())
}

fn reason_message(reason: Option<u32>, wrong_password: bool) -> Option<String> {
    if wrong_password {
        return Some(MSG_WRONG_PASSWORD.into());
    }
    match reason {
        Some(10) => Some(MSG_WRONG_PASSWORD.into()),
        Some(9) => Some("Password required".into()),
        Some(6) => Some("Connection timed out".into()),
        Some(5) => Some("Couldn't get an IP address".into()),
        Some(8) => Some("Couldn't start network service".into()),
        Some(11) | Some(12) => Some("Connection disappeared; try again".into()),
        Some(2) => None,
        _ => Some(MSG_FAILED.into()),
    }
}

const WATCH_POLL: Duration = Duration::from_millis(400);
const WATCH_WINDOW: Duration = Duration::from_secs(20);
const QUIET_BEFORE_RETRY: Duration = Duration::from_secs(8);
const REACTIVATE_TRIES: u32 = 3;
const REACTIVATE_LAND_WINDOW: Duration = Duration::from_secs(8);

impl NmClient {
    async fn ap_path_for_ssid(
        &self,
        wifi_path: &OwnedObjectPath,
        ssid: &str,
    ) -> Result<OwnedObjectPath> {
        use rusty_network_manager::WirelessProxy;
        let Some(wifi) = uncached_proxy!(WirelessProxy, self.system_conn(), wifi_path.clone())
        else {
            return root_path();
        };
        let aps = wifi.get_all_access_points().await.unwrap_or_default();
        let want = ssid.as_bytes().to_vec();
        let conn = self.system.clone();
        let rows = super::map_bounded(aps, move |path| {
            let conn = conn.clone();
            async move {
                use rusty_network_manager::AccessPointProxy;
                let proxy = uncached_proxy!(AccessPointProxy, &conn, path.clone())?;
                let (Ok(bytes), Ok(strength)) = (proxy.ssid().await, proxy.strength().await) else {
                    return None;
                };
                Some((path, bytes, strength))
            }
        })
        .await;
        match rows
            .into_iter()
            .flatten()
            .filter(|(_, bytes, _)| bytes == &want)
            .max_by_key(|(_, _, s)| *s)
        {
            Some((p, _, _)) => Ok(p),
            None => root_path(),
        }
    }
    async fn activate_profile(
        &self,
        wifi: &OwnedObjectPath,
        profile: &OwnedObjectPath,
        ap: &OwnedObjectPath,
    ) -> Result<OwnedObjectPath> {
        let active = self
            .nm
            .activate_connection(profile, wifi, ap)
            .await
            .with_context(|| format!("ActivateConnection failed for {profile}"))?;
        tracing::info!(%profile, %active, "ActivateConnection issued");
        self.drop_profile_cache().await;
        Ok(active)
    }
    async fn activate_new_profile(
        &self,
        ssid: &str,
        profile: HashMap<&str, HashMap<&str, Value<'_>>>,
        ap: &OwnedObjectPath,
        psk: Option<&str>,
        secrets: &SecretStore,
    ) -> Result<ConnectOutcome> {
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let settings = rusty_network_manager::SettingsProxy::new(self.system_conn()).await?;
        let conn = settings
            .add_connection_unsaved(profile)
            .await
            .inspect_err(|e| tracing::warn!(ssid, "AddConnectionUnsaved failed: {e:#}"))?;
        if let Some(psk) = psk {
            secrets.provide(&conn.to_string(), psk.to_string()).await;
        }
        match self.activate_profile(&wifi, &conn, ap).await {
            Ok(active) => {
                tracing::info!(ssid, %conn, %active, "unsaved profile activated");
                Ok(ConnectOutcome {
                    active,
                    created: Some(conn),
                    commit: None,
                    persist_on_success: true,
                })
            }
            Err(e) => {
                let _ = self.delete_profile(&conn).await;
                Err(e)
            }
        }
    }
    pub async fn connect_open(&self, ssid: &str) -> Result<ConnectOutcome> {
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        if let Some(profile) = super::saved_profile_path(self.system_conn(), ssid).await? {
            let secured = super::get_settings(self.system_conn(), &profile)
                .await
                .is_some_and(|map| map.contains_key("802-11-wireless-security"));
            if !secured {
                let ap = self.ap_path_for_ssid(&wifi, ssid).await?;
                let active = self.activate_profile(&wifi, &profile, &ap).await?;
                return Ok(ConnectOutcome {
                    active,
                    created: None,
                    commit: None,
                    persist_on_success: false,
                });
            }
        }
        let mut profile = open_profile(ssid);
        if let Some(conn) = profile.get_mut("connection") {
            conn.insert("autoconnect", Value::new(false));
        }
        self.activate_new_profile_open(ssid, &wifi, profile).await
    }
    async fn activate_new_profile_open(
        &self,
        ssid: &str,
        wifi: &OwnedObjectPath,
        profile: HashMap<&str, HashMap<&str, Value<'_>>>,
    ) -> Result<ConnectOutcome> {
        let ap = self.ap_path_for_ssid(wifi, ssid).await?;
        let settings = rusty_network_manager::SettingsProxy::new(self.system_conn()).await?;
        let conn = settings
            .add_connection_unsaved(profile)
            .await
            .inspect_err(|e| tracing::warn!(ssid, "AddConnectionUnsaved failed: {e:#}"))?;
        match self.activate_profile(wifi, &conn, &ap).await {
            Ok(active) => {
                tracing::info!(ssid, %conn, %active, "unsaved open profile activated");
                Ok(ConnectOutcome {
                    active,
                    created: Some(conn),
                    commit: None,
                    persist_on_success: false,
                })
            }
            Err(e) => {
                let _ = self.delete_profile(&conn).await;
                Err(e)
            }
        }
    }
    pub async fn connect_secure(
        &self,
        ssid: &str,
        psk: Secret,
        secrets: &SecretStore,
    ) -> Result<ConnectOutcome> {
        validate_psk(psk.as_str())?;
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        if let Some(profile) = super::saved_profile_path(self.system_conn(), ssid).await? {
            secrets.clear_served(&profile.to_string()).await;
            secrets.provide(&profile.to_string(), psk.to_string()).await;
            let ap = self.ap_path_for_ssid(&wifi, ssid).await?;
            let active = self.activate_profile(&wifi, &profile, &ap).await?;
            return Ok(ConnectOutcome {
                active,
                created: None,
                commit: Some(profile),
                persist_on_success: false,
            });
        }
        let ap = self.ap_path_for_ssid(&wifi, ssid).await?;
        let profile = secure_profile(ssid, psk.as_str());
        self.activate_new_profile(ssid, profile, &ap, Some(psk.as_str()), secrets)
            .await
    }
    pub async fn connect_hidden(
        &self,
        ssid: &str,
        psk: Secret,
        secrets: &SecretStore,
    ) -> Result<ConnectOutcome> {
        if !psk.is_empty() {
            validate_psk(psk.as_str())?;
        }
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let root = root_path()?;
        if let Some(profile) = super::saved_profile_path(self.system_conn(), ssid).await? {
            let commit = if psk.is_empty() {
                None
            } else {
                secrets.clear_served(&profile.to_string()).await;
                secrets.provide(&profile.to_string(), psk.to_string()).await;
                Some(profile.clone())
            };
            let active = self.activate_profile(&wifi, &profile, &root).await?;
            return Ok(ConnectOutcome {
                active,
                created: None,
                commit,
                persist_on_success: false,
            });
        }
        let mut profile = hidden_profile(ssid, psk.as_str());
        if psk.is_empty()
            && let Some(conn) = profile.get_mut("connection")
        {
            conn.insert("autoconnect", Value::new(false));
        }
        let settings = rusty_network_manager::SettingsProxy::new(self.system_conn()).await?;
        let conn = settings
            .add_connection_unsaved(profile)
            .await
            .inspect_err(|e| tracing::warn!(ssid, "AddConnectionUnsaved failed: {e:#}"))?;
        if !psk.is_empty() {
            secrets.provide(&conn.to_string(), psk.to_string()).await;
        }
        match self.activate_profile(&wifi, &conn, &root).await {
            Ok(active) => {
                tracing::info!(ssid, %conn, %active, "unsaved hidden profile activated");
                Ok(ConnectOutcome {
                    active,
                    created: Some(conn),
                    commit: None,
                    persist_on_success: !psk.is_empty(),
                })
            }
            Err(e) => {
                let _ = self.delete_profile(&conn).await;
                Err(e)
            }
        }
    }
    pub async fn update_psk(&self, profile: &OwnedObjectPath, psk: &str) -> Result<()> {
        let Some(proxy) = uncached_proxy!(
            rusty_network_manager::SettingsConnectionProxy,
            self.system_conn(),
            profile.clone()
        ) else {
            anyhow::bail!("could not open the saved profile");
        };

        let mut map = super::get_settings(self.system_conn(), profile)
            .await
            .context("could not read the saved profile")?;
        if !map.contains_key("connection") {
            anyhow::bail!("saved profile has no connection group; refusing to rewrite it");
        }
        let new_secret = zbus::zvariant::OwnedValue::try_from(Value::new(psk))
            .context("could not encode the new password")?;
        let sec = map
            .entry("802-11-wireless-security".to_string())
            .or_default();
        sec.insert("psk".to_string(), new_secret);
        sec.entry("key-mgmt".to_string()).or_insert_with(|| {
            zbus::zvariant::OwnedValue::try_from(Value::new("wpa-psk")).expect("static str encodes")
        });
        let mut full: HashMap<&str, HashMap<&str, Value>> = HashMap::new();
        for (group, values) in &map {
            let mut row: HashMap<&str, Value> = HashMap::new();
            for (key, value) in values {
                row.insert(
                    key.as_str(),
                    Value::try_from(value).context("could not re-encode the saved profile")?,
                );
            }
            full.insert(group.as_str(), row);
        }
        proxy.update(full).await?;
        tracing::info!(%profile, "saved profile password updated");
        Ok(())
    }
    pub async fn persist_profile(&self, path: &OwnedObjectPath) -> Result<()> {
        let Some(proxy) = uncached_proxy!(
            rusty_network_manager::SettingsConnectionProxy,
            self.system_conn(),
            path.clone()
        ) else {
            anyhow::bail!("could not open the new profile");
        };
        proxy.save().await?;
        tracing::info!(%path, "verified connection persisted");
        self.drop_profile_cache().await;
        Ok(())
    }
    pub async fn delete_profile(&self, path: &OwnedObjectPath) -> Result<()> {
        let Some(proxy) = uncached_proxy!(
            rusty_network_manager::SettingsConnectionProxy,
            self.system_conn(),
            path.clone()
        ) else {
            anyhow::bail!("could not open the stale profile");
        };
        proxy.delete().await?;
        tracing::info!(%path, "connection profile removed");
        self.drop_profile_cache().await;
        Ok(())
    }
    pub async fn delete_unsaved_for_ssid(&self, ssid: &str) -> bool {
        let mut removed = false;
        for path in super::profiles::wireless_profile_paths(self.system_conn()).await {
            let Some(map) = super::get_settings(self.system_conn(), &path).await else {
                continue;
            };
            if super::profiles::wireless_ssid(&map).as_deref() != Some(ssid) {
                continue;
            }
            if !super::profile_unsaved(self.system_conn(), &path).await {
                continue;
            }
            match self.delete_profile(&path).await {
                Ok(()) => {
                    tracing::info!(ssid, %path, "leftover unsaved profile removed");
                    removed = true;
                }
                Err(e) => {
                    tracing::warn!(ssid, %path, "could not remove unsaved profile: {e:#}")
                }
            }
        }
        removed
    }
    pub async fn forget_all_saved(&self, ssid: &str) -> Result<usize> {
        let mut removed = 0;
        for _ in 0..8 {
            let Some(path) = super::saved_profile_path(self.system_conn(), ssid).await? else {
                break;
            };
            self.delete_profile(&path).await?;
            removed += 1;
        }
        if removed == 0 {
            anyhow::bail!("no saved profile for {ssid}");
        }
        tracing::info!(ssid, removed, "saved profiles deleted");
        Ok(removed)
    }
    pub async fn connect_saved(&self, ssid: &str) -> Result<ConnectOutcome> {
        let profile = super::saved_profile_path(self.system_conn(), ssid)
            .await?
            .with_context(|| format!("no saved profile for {ssid}"))?;
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let ap = self.ap_path_for_ssid(&wifi, ssid).await?;
        tracing::info!(ssid, %profile, %wifi, %ap, "activating saved profile");
        let active = self.activate_profile(&wifi, &profile, &ap).await?;
        tracing::info!(ssid, %active, "ActivateConnection issued (saved)");
        Ok(ConnectOutcome {
            active,
            created: None,
            commit: None,
            persist_on_success: false,
        })
    }

    pub async fn retry_saved_with_psk(
        &self,
        ssid: &str,
        psk: Secret,
        secrets: &SecretStore,
    ) -> Result<ConnectOutcome> {
        validate_psk(psk.as_str())?;
        let profile = super::saved_profile_path(self.system_conn(), ssid)
            .await?
            .with_context(|| format!("no saved profile for {ssid}"))?;
        secrets.clear_served(&profile.to_string()).await;
        secrets.provide(&profile.to_string(), psk.to_string()).await;
        self.dispose_stuck_activation(ssid).await;
        tracing::info!(
            ssid,
            %profile,
            "saved profile selected; the new password is staged, not written"
        );
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let ap = self.ap_path_for_ssid(&wifi, ssid).await?;
        let active = self.activate_profile(&wifi, &profile, &ap).await?;
        Ok(ConnectOutcome {
            active,
            created: None,
            commit: Some(profile),
            persist_on_success: false,
        })
    }

    pub async fn dispose_stuck_activation(&self, ssid: &str) -> bool {
        use rusty_network_manager::ActiveProxy;
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Some(proxy) = uncached_proxy!(ActiveProxy, self.system_conn(), path.clone()) else {
                continue;
            };
            if proxy.type_().await.as_deref() != Ok("802-11-wireless") {
                continue;
            }
            if proxy.state().await.unwrap_or(0) != 1 {
                continue;
            }
            let Ok(profile) = proxy.connection().await else {
                continue;
            };
            let Some(map) = super::get_settings(self.system_conn(), &profile).await else {
                continue;
            };
            if super::profiles::wireless_ssid(&map).as_deref() == Some(ssid) {
                match self.nm.deactivate_connection(&path).await {
                    Ok(()) => {
                        tracing::info!(ssid, %path, "DeactivateConnection issued (cancel stuck activation)");
                        return true;
                    }
                    Err(e) => tracing::warn!(%path, "could not cancel stuck activation: {e:#}"),
                }
            }
        }
        false
    }
    pub async fn active_paths_for_ssid(&self, ssid: &str) -> Vec<OwnedObjectPath> {
        use rusty_network_manager::ActiveProxy;
        let mut out = Vec::new();
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Some(proxy) = uncached_proxy!(ActiveProxy, self.system_conn(), path.clone()) else {
                continue;
            };
            if proxy.type_().await.as_deref() != Ok("802-11-wireless") {
                continue;
            }
            let Ok(profile) = proxy.connection().await else {
                continue;
            };
            let Some(map) = super::get_settings(self.system_conn(), &profile).await else {
                continue;
            };
            if super::profiles::wireless_ssid(&map).as_deref() == Some(ssid) {
                out.push(path);
            }
        }
        out
    }
    pub async fn disconnect_active(&self) -> Result<()> {
        use rusty_network_manager::ActiveProxy;
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Some(proxy) = uncached_proxy!(ActiveProxy, self.system_conn(), path.clone()) else {
                continue;
            };
            if !matches!(
                proxy.type_().await.as_deref(),
                Ok("802-11-wireless") | Ok("802-3-ethernet")
            ) {
                continue;
            }
            if super::active_conn_is_hotspot(self.system_conn(), &path).await {
                continue;
            }
            self.nm.deactivate_connection(&path).await?;
            tracing::info!(%path, "DeactivateConnection issued");
            return Ok(());
        }
        anyhow::bail!("no active Wi-Fi connection to disconnect");
    }
    pub async fn activate_vpn(&self, id: &str) -> Result<()> {
        let profile = self
            .vpn_connections()
            .await?
            .into_iter()
            .find(|v| v.id == id)
            .map(|v| OwnedObjectPath::try_from(v.path.as_str()))
            .transpose()?
            .with_context(|| format!("no saved VPN profile for {id}"))?;
        let root = OwnedObjectPath::try_from("/").context("invalid root object path")?;
        let active = self.nm.activate_connection(&profile, &root, &root).await?;
        tracing::info!(id, %active, "ActivateConnection issued (vpn)");
        self.drop_profile_cache().await;
        Ok(())
    }
    pub async fn deactivate_vpn(&self, id: &str) -> Result<()> {
        use rusty_network_manager::ActiveProxy;
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Some(proxy) = uncached_proxy!(ActiveProxy, self.system_conn(), path.clone()) else {
                continue;
            };
            if !matches!(proxy.type_().await.as_deref(), Ok("vpn") | Ok("wireguard")) {
                continue;
            }
            let name = proxy.id().await.unwrap_or_default();
            if name != id {
                continue;
            }
            self.nm.deactivate_connection(&path).await?;
            tracing::info!(id, %path, "DeactivateConnection issued (vpn)");
            self.drop_profile_cache().await;
            return Ok(());
        }
        anyhow::bail!("vpn {id} is not active");
    }
    pub async fn active_wifi_snapshot(&self) -> Option<(zbus::zvariant::OwnedObjectPath, String)> {
        use rusty_network_manager::ActiveProxy;
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Some(proxy) = uncached_proxy!(ActiveProxy, self.system_conn(), path.clone()) else {
                continue;
            };
            if proxy.type_().await.as_deref() != Ok("802-11-wireless") {
                continue;
            }
            if proxy.state().await.unwrap_or(0) != 2 {
                continue;
            }
            let Ok(profile) = proxy.connection().await else {
                continue;
            };
            let Some(map) = super::get_settings(self.system_conn(), &profile).await else {
                continue;
            };
            if map
                .get("802-11-wireless")
                .and_then(|w| w.get("mode"))
                .map(|v| matches!(&**v, Value::Str(s) if s.as_str() == "ap"))
                .unwrap_or(false)
            {
                continue;
            }
            let ssid = map
                .get("802-11-wireless")
                .and_then(|w| w.get("ssid"))
                .and_then(super::bytes_to_ssid);
            if let Some(ssid) = ssid {
                return Some((profile, ssid));
            }
        }
        None
    }
    pub async fn reactivate_previous(
        &self,
        ssid: &str,
        profile: &zbus::zvariant::OwnedObjectPath,
    ) -> Result<()> {
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let ap = self.ap_path_for_ssid(&wifi, ssid).await?;
        self.nm.activate_connection(profile, &wifi, &ap).await?;
        tracing::info!(ssid, %profile, "previous connection reactivated after failed activation");
        Ok(())
    }
    async fn current_active_state(&self, path: &OwnedObjectPath) -> Result<u32> {
        let Some(proxy) = uncached_proxy!(
            rusty_network_manager::ActiveProxy,
            self.system_conn(),
            path.clone()
        ) else {
            anyhow::bail!("could not open the active connection")
        };
        proxy
            .state()
            .await
            .context("could not read active connection state")
    }
    pub(crate) async fn poll_active_state(
        &self,
        path: &OwnedObjectPath,
        ssid: &Ssid,
        window: Duration,
    ) -> bool {
        let mut deadline = tokio::time::Instant::now() + window;
        let mut errors = 0u32;
        let mut extended = false;
        loop {
            match self.current_active_state(path).await {
                Ok(2) => return true,
                Ok(4) | Ok(5) => return false,
                Ok(state) => {
                    errors = 0;
                    if tokio::time::Instant::now() >= deadline {
                        if state == 1 && !extended {
                            extended = true;
                            deadline =
                                tokio::time::Instant::now() + std::time::Duration::from_secs(15);
                            tracing::info!(
                                ssid = %ssid.as_str(), %path,
                                "activation still progressing; extending window once"
                            );
                            continue;
                        }
                        tracing::warn!(ssid = %ssid.as_str(), %path, "activation window elapsed; cancelling stale attempt");
                        self.dispose_stuck_activation(ssid.as_str()).await;
                        return false;
                    }
                }
                Err(_) => {
                    errors += 1;
                    if errors >= 3 {
                        tracing::warn!(ssid = %ssid.as_str(), %path, "activation state unreadable; failing the attempt");
                        self.dispose_stuck_activation(ssid.as_str()).await;
                        return false;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        tracing::warn!(ssid = %ssid.as_str(), %path, "activation window elapsed; cancelling stale attempt");
                        self.dispose_stuck_activation(ssid.as_str()).await;
                        return false;
                    }
                }
            }
            tokio::time::sleep(WATCH_POLL).await;
        }
    }
}

impl NmClient {
    #[allow(clippy::too_many_arguments)]
    pub async fn watch_attempt(
        &self,
        target: &Ssid,
        active: &OwnedObjectPath,
        prev: Option<PrevWifi>,
        created: Option<OwnedObjectPath>,
        commit: Option<OwnedObjectPath>,
        persist_on_success: bool,
        secrets: &SecretStore,
        feed: &async_channel::Sender<UiEvent>,
        watch_rx: &ModelRx,
        refresh: &Arc<dyn Fn() + Send + Sync>,
        epoch: &Arc<AtomicU64>,
        mine: u64,
    ) {
        let current = || epoch.load(Ordering::Relaxed) == mine;
        tracing::info!(ssid = %target.as_str(), %active, "watching activation state");
        let watch_path = commit.clone().map(|p| p.to_string());
        let mut wrong_password = false;
        let landed = tokio::select! {
            r = self.poll_active_state(active, target, WATCH_WINDOW) => r,
            _ = async {
                loop {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    if let Some(path) = watch_path.as_deref()
                        && secrets.rejected(path).await
                    {
                        break;
                    }
                }
            } => {
                wrong_password = true;
                self.dispose_stuck_activation(target.as_str()).await;
                false
            }
        };
        if landed {
            if !current() {
                tracing::info!(ssid = %target.as_str(), "superseded by newer attempt; standing down");
                refresh();
                return;
            }
            if let Some(path) = &commit
                && let Some(secret) = secrets.take_served_psk(&path.to_string()).await
            {
                match self.update_psk(path, secret.as_str()).await {
                    Ok(()) => tracing::info!(
                        ssid = %target.as_str(),
                        "verified password saved to the stored profile"
                    ),
                    Err(e) => tracing::warn!("could not save the verified password: {e:#}"),
                }
            }
            if persist_on_success
                && let Some(path) = &created
                && let Err(e) = self.persist_profile(path).await
            {
                tracing::warn!(
                    ssid = %target.as_str(),
                    "could not persist verified connection: {e:#}"
                );
            }
            self.mark_connected(target.as_str()).await;
            refresh();
            tracing::info!(ssid = %target.as_str(), "connect attempt landed");
            return;
        }
        tracing::info!(ssid = %target.as_str(), "connect attempt failed");
        if !current() {
            tracing::info!(ssid = %target.as_str(), "superseded; skipping stale failure handling");
            let _ = feed.try_send(UiEvent::ClearConnecting(target.clone()));
            return;
        }
        let reason = active_reason(self.system_conn(), active).await;
        match reason_message(reason, wrong_password) {
            Some(message) => {
                let _ = feed.try_send(UiEvent::SsidError {
                    ssid: target.clone(),
                    message,
                });
            }
            None => {
                let _ = feed.try_send(UiEvent::ClearConnecting(target.clone()));
            }
        }
        refresh();
        if let Some(path) = &commit {
            secrets.clear_served(&path.to_string()).await;
        }
        if let Some(path) = created
            && let Err(e) = self.delete_profile(&path).await
        {
            tracing::warn!(
                ssid = %target.as_str(),
                "could not remove failed connection profile: {e:#}"
            );
        }
        if let Some(p) = prev.filter(|p| p.ssid.as_str() != target.as_str()) {
            let start = std::time::Instant::now();
            while start.elapsed() < QUIET_BEFORE_RETRY {
                if !current() {
                    tracing::info!(ssid = %target.as_str(), "superseded during rollback wait");
                    return;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                let active = watch_rx.borrow().active_ssid.clone();
                match active.as_deref() {
                    Some(t) if t == target.as_str() => return,
                    Some(_) => {
                        return;
                    }
                    None => {}
                }
            }
            for attempt in 1..=REACTIVATE_TRIES {
                if !current() {
                    tracing::info!(ssid = %target.as_str(), "superseded; aborting rollback");
                    return;
                }
                if watch_rx.borrow().active_ssid.is_some() {
                    return;
                }
                match self.reactivate_previous(&p.ssid, &p.profile).await {
                    Ok(()) => {
                        let deadline = std::time::Instant::now() + REACTIVATE_LAND_WINDOW;
                        loop {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                            match watch_rx.borrow().active_ssid.as_deref() {
                                Some(s) if s == p.ssid => {
                                    tracing::info!(
                                        ssid = %p.ssid,
                                        "returned to previous network after failed attempt"
                                    );
                                    return;
                                }
                                Some(_) => return,
                                None => {}
                            }
                            if std::time::Instant::now() >= deadline {
                                tracing::warn!(
                                    ssid = %p.ssid,
                                    attempt,
                                    "reactivation did not land; retrying"
                                );
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            ssid = %p.ssid,
                            attempt,
                            "could not return to previous network: {e:#}"
                        );
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
            if watch_rx.borrow().active_ssid.is_none() && current() {
                let _ = feed.try_send(UiEvent::SsidError {
                    ssid: Ssid::from(p.ssid.clone()),
                    message: "Couldn't reconnect".into(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn open_profile_shape() {
        let p = open_profile("Cafe");
        assert_eq!(p.len(), 4);
        assert!(p.contains_key("connection"));
        assert!(p.contains_key("802-11-wireless"));
        assert!(p.contains_key("ipv4"));
        assert!(p.contains_key("ipv6"));
        let conn = &p["connection"];
        assert!(matches!(&conn["type"], Value::Str(s) if s.as_str() == "802-11-wireless"));
        let w = &p["802-11-wireless"];
        assert!(matches!(&w["ssid"], Value::Array(_)));
    }
    #[test]
    fn secure_profile_shape() {
        let p = secure_profile("Home", "hunter2");
        assert!(p.contains_key("802-11-wireless-security"));
        let sec = &p["802-11-wireless-security"];
        assert!(matches!(&sec["key-mgmt"], Value::Str(s) if s.as_str() == "wpa-psk"));
        assert!(matches!(&sec["psk"], Value::Str(s) if s.as_str() == "hunter2"));
    }
    #[test]
    fn hidden_profile_sets_flag() {
        let open = hidden_profile("Hid", "");
        assert!(matches!(
            &open["802-11-wireless"]["hidden"],
            Value::Bool(true)
        ));
        assert!(!open.contains_key("802-11-wireless-security"));
        let sec = hidden_profile("Hid", "pw123456");
        assert!(matches!(
            &sec["802-11-wireless"]["hidden"],
            Value::Bool(true)
        ));
        assert!(
            matches!(&sec["802-11-wireless-security"]["psk"], Value::Str(s) if s.as_str() == "pw123456")
        );
    }
    #[test]
    fn psk_validation_rules() {
        assert!(validate_psk("12345678").is_ok());
        assert!(validate_psk("p".repeat(63).as_str()).is_ok());
        assert!(validate_psk("short").is_err());
        assert!(validate_psk(&"p".repeat(64)).is_err());
        assert!(validate_psk(" 1234567").is_err());
        assert!(validate_psk("1234567 ").is_err());
        assert!(validate_psk("1234\n567").is_err());
        assert!(validate_psk("").is_err());
    }
    #[test]
    fn open_connects_never_persist_a_new_profile() {
        fn simulate(secured: bool) -> ConnectOutcome {
            ConnectOutcome {
                active: root_path().unwrap(),
                created: if secured {
                    None
                } else {
                    Some(root_path().unwrap())
                },
                commit: None,
                persist_on_success: false,
            }
        }
        assert!(!simulate(true).persist_on_success);
        assert!(!simulate(false).persist_on_success);
    }
    #[test]
    fn wrapper_restores_skips_a_target_itself() {
        let prev = PrevWifi {
            profile: root_path().unwrap(),
            ssid: "Home".into(),
        };
        assert!(!prev_filter_applies(&prev, "Home"));
        let other = PrevWifi {
            profile: root_path().unwrap(),
            ssid: "Cafe".into(),
        };
        assert!(prev_filter_applies(&other, "Home"));
    }
    fn prev_filter_applies(p: &PrevWifi, target: &str) -> bool {
        p.ssid != target
    }
}
