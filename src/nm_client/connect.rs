use super::NmClient;
use crate::state::ModelRx;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::sync::Arc;
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
impl NmClient {
    async fn ap_path_for_ssid(
        &self,
        wifi_path: &OwnedObjectPath,
        ssid: &str,
    ) -> Result<OwnedObjectPath> {
        use rusty_network_manager::WirelessProxy;
        let Ok(wifi) = WirelessProxy::new_from_path(wifi_path.clone(), self.system_conn()).await
        else {
            return root_path();
        };
        let aps = wifi.get_all_access_points().await.unwrap_or_default();
        let want = ssid.as_bytes().to_vec();
        let rows = super::map_bounded(aps, |path| {
            let conn = self.system.clone();
            async move {
                use rusty_network_manager::AccessPointProxy;
                let Ok(proxy) = AccessPointProxy::new_from_path(path.clone(), &conn).await else {
                    return None;
                };
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
    pub async fn connect_open(&self, ssid: &str) -> Result<()> {
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let ap = self.ap_path_for_ssid(&wifi, ssid).await?;
        let existing = super::saved_profile_path(self.system_conn(), ssid).await?;
        let active = if let Some(profile) = existing {
            self.nm.activate_connection(&profile, &wifi, &ap).await?
        } else {
            let profile = open_profile(ssid);
            let (conn, active) = self
                .nm
                .add_and_activate_connection(profile, &wifi, &ap)
                .await?;
            tracing::info!(ssid, %conn, "AddAndActivateConnection issued (open, new profile)");
            active
        };
        tracing::info!(ssid, %active, "ActivateConnection issued (open)");
        Ok(())
    }
    pub async fn connect_secure(&self, ssid: &str, psk: &str) -> Result<()> {
        validate_psk(psk)?;
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let ap = self.ap_path_for_ssid(&wifi, ssid).await?;
        let existing = super::saved_profile_path(self.system_conn(), ssid).await?;
        let active = if let Some(profile) = existing {
            self.update_psk(&profile, psk).await?;
            self.nm.activate_connection(&profile, &wifi, &ap).await?
        } else {
            let profile = secure_profile(ssid, psk);
            let (conn, active) = self
                .nm
                .add_and_activate_connection(profile, &wifi, &ap)
                .await?;
            tracing::info!(ssid, %conn, "AddAndActivateConnection issued (secure, new profile)");
            active
        };
        tracing::info!(ssid, %active, "ActivateConnection issued (secure)");
        Ok(())
    }
    pub async fn connect_hidden(&self, ssid: &str, psk: &str) -> Result<()> {
        if !psk.is_empty() {
            validate_psk(psk)?;
        }
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let existing = super::saved_profile_path(self.system_conn(), ssid).await?;
        let profile = if let Some(profile) = existing {
            if !psk.is_empty() {
                self.update_psk(&profile, psk).await?;
            }
            profile
        } else {
            let profile = hidden_profile(ssid, psk);
            let root = root_path()?;
            let (conn, _active) = self
                .nm
                .add_and_activate_connection(profile, &wifi, &root)
                .await?;
            tracing::info!(ssid, %conn, "AddAndActivateConnection issued (hidden, new profile)");
            return Ok(());
        };
        let root = root_path()?;
        let active = self.nm.activate_connection(&profile, &wifi, &root).await?;
        tracing::info!(ssid, %active, "ActivateConnection issued (hidden)");
        Ok(())
    }
    async fn update_psk(&self, profile: &OwnedObjectPath, psk: &str) -> Result<()> {
        let proxy = rusty_network_manager::SettingsConnectionProxy::new_from_path(
            profile.clone(),
            self.system_conn(),
        )
        .await?;
        let mut sec = HashMap::new();
        sec.insert("psk", Value::new(psk));
        let mut outer: HashMap<&str, HashMap<&str, Value>> = HashMap::new();
        outer.insert("802-11-wireless-security", sec);
        proxy.update(outer).await?;
        tracing::info!(%profile, "saved profile password updated");
        Ok(())
    }
    pub async fn forget_saved(&self, ssid: &str) -> Result<()> {
        let path = super::saved_profile_path(self.system_conn(), ssid)
            .await?
            .with_context(|| format!("no saved profile for {ssid}"))?;
        let proxy = rusty_network_manager::SettingsConnectionProxy::new_from_path(
            path.clone(),
            self.system_conn(),
        )
        .await?;
        proxy.delete().await?;
        tracing::info!(ssid, %path, "saved profile deleted");
        Ok(())
    }
    pub async fn connect_saved(&self, ssid: &str) -> Result<()> {
        let profile = super::saved_profile_path(self.system_conn(), ssid)
            .await?
            .with_context(|| format!("no saved profile for {ssid}"))?;
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let ap = self.ap_path_for_ssid(&wifi, ssid).await?;
        let active = self.nm.activate_connection(&profile, &wifi, &ap).await?;
        tracing::info!(ssid, %active, "ActivateConnection issued (saved)");
        Ok(())
    }
    pub async fn disconnect_active(&self) -> Result<()> {
        use rusty_network_manager::ActiveProxy;
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Ok(proxy) = ActiveProxy::new_from_path(path.clone(), self.system_conn()).await
            else {
                continue;
            };
            if proxy
                .type_()
                .await
                .map(|t| t == "802-11-wireless")
                .unwrap_or(false)
            {
                if super::active_conn_is_hotspot(self.system_conn(), &path).await {
                    continue;
                }
                self.nm.deactivate_connection(&path).await?;
                tracing::info!(%path, "DeactivateConnection issued");
                return Ok(());
            }
        }
        anyhow::bail!("no active Wi-Fi connection to disconnect");
    }
    pub async fn activate_vpn(&self, id: &str) -> Result<()> {
        let profile = super::profiles::vpn_profile_path(self.system_conn(), id)
            .await?
            .with_context(|| format!("no saved VPN profile for {id}"))?;
        let root = OwnedObjectPath::try_from("/").context("invalid root object path")?;
        let active = self.nm.activate_connection(&profile, &root, &root).await?;
        tracing::info!(id, %active, "ActivateConnection issued (vpn)");
        Ok(())
    }
    pub async fn deactivate_vpn(&self, id: &str) -> Result<()> {
        use rusty_network_manager::ActiveProxy;
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Ok(proxy) = ActiveProxy::new_from_path(path.clone(), self.system_conn()).await
            else {
                continue;
            };
            if proxy.type_().await.as_deref() != Ok("vpn") {
                continue;
            }
            let name = proxy.id().await.unwrap_or_default();
            if name != id {
                continue;
            }
            self.nm.deactivate_connection(&path).await?;
            tracing::info!(id, %path, "DeactivateConnection issued (vpn)");
            return Ok(());
        }
        anyhow::bail!("vpn {id} is not active");
    }
    pub async fn active_wifi_snapshot(&self) -> Option<(zbus::zvariant::OwnedObjectPath, String)> {
        use rusty_network_manager::ActiveProxy;
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Ok(proxy) = ActiveProxy::new_from_path(path.clone(), self.system_conn()).await
            else {
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
    pub async fn restore_previous(&self, prev: Option<PrevWifi>) -> Result<bool> {
        if let Some(PrevWifi { profile, ssid }) = prev {
            self.reactivate_previous(&ssid, &profile).await?;
            return Ok(true);
        }
        Ok(false)
    }
    pub async fn restore_by_priority(&self, avoid: &str) -> Result<Option<String>> {
        for (ssid, priority) in self.autoconnect_candidates().await {
            if ssid == avoid {
                continue;
            }
            let Some(profile) =
                super::profiles::saved_profile_path(self.system_conn(), &ssid).await?
            else {
                continue;
            };
            if let Err(e) = self.reactivate_previous(&ssid, &profile).await {
                tracing::warn!("priority fallback to {ssid} (p={priority}) failed: {e:#}");
                continue;
            }
            tracing::info!(
                ssid,
                priority,
                "fell back to highest-priority saved network"
            );
            return Ok(Some(ssid));
        }
        Ok(None)
    }
}
pub struct PrevWifi {
    pub profile: zbus::zvariant::OwnedObjectPath,
    pub ssid: String,
}
pub fn spawn_restore_guard(
    client: Arc<NmClient>,
    mut watch_rx: ModelRx,
    feed: async_channel::Sender<crate::state::UiEvent>,
    target_ssid: String,
    prev: Option<PrevWifi>,
    window: Duration,
    registry: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
) {
    let prev_ssid = prev.as_ref().map(|p| p.ssid.clone());
    if prev.is_none() {
        tokio::spawn(async move {
            let _ = &registry;
            match client.restore_by_priority(&target_ssid).await {
                Ok(Some(ssid)) => {
                    let _ = feed.try_send(crate::state::UiEvent::SsidError {
                        ssid: target_ssid,
                        message: format!("Could not connect. Reconnected to “{ssid}”."),
                    });
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("priority fallback failed: {e:#}"),
            }
        });
        return;
    }
    let prev = prev.expect("checked above");
    tokio::spawn(async move {
        {
            let mut seen = registry.lock().await;
            if !seen.insert(target_ssid.clone()) {
                return;
            }
        }
        let guard = GuardGuard {
            registry: registry.clone(),
            ssid: target_ssid.clone(),
        };
        let _ = &guard;
        let deadline = tokio::time::Instant::now() + window;
        let mut none_since: Option<tokio::time::Instant> = None;
        let mut saw_disconnect = false;
        loop {
            {
                let m = watch_rx.borrow();
                match m.active_ssid.as_deref() {
                    Some(s) if s == target_ssid.as_str() => return,
                    Some(s) if s == prev.ssid.as_str() && saw_disconnect => return,
                    Some(_) if saw_disconnect => return,
                    None => {
                        saw_disconnect = true;
                        if none_since.is_none() {
                            none_since = Some(tokio::time::Instant::now());
                        }
                        if none_since
                            .as_ref()
                            .map(|t| t.elapsed() >= Duration::from_secs(8))
                            .unwrap_or(false)
                        {
                            break;
                        }
                    }
                    _ => {}
                }
                if !m.wifi_enabled {
                    return;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            if tokio::time::timeout(Duration::from_millis(500), watch_rx.changed())
                .await
                .is_err()
            {
                return;
            }
        }
        match client.reactivate_previous(&prev.ssid, &prev.profile).await {
            Ok(()) => {
                let _ = feed.try_send(crate::state::UiEvent::SsidError {
                    ssid: target_ssid.clone(),
                    message: format!(
                        "Could not connect to “{target_ssid}”. Reconnected to “{}”.",
                        prev.ssid
                    ),
                });
            }
            Err(e) => {
                tracing::warn!("failed to restore previous network {}: {e:#}", prev.ssid);
                let target = target_ssid.clone();
                match client.restore_by_priority(&target_ssid).await {
                    Ok(Some(ssid)) => {
                        let _ = feed.try_send(crate::state::UiEvent::SsidError {
                            ssid: target,
                            message: format!(
                                "Could not connect to “{target_ssid}”. Reconnected to “{ssid}”."
                            ),
                        });
                    }
                    _ => {
                        let _ = feed.try_send(crate::state::UiEvent::SsidError {
                            ssid: target,
                            message: format!(
                                "Could not connect to “{target_ssid}”. Reconnect to “{}” manually.",
                                prev_ssid.unwrap_or_else(|| "a saved network".into())
                            ),
                        });
                    }
                }
            }
        }
    });
}
struct GuardGuard {
    registry: Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>,
    ssid: String,
}
impl Drop for GuardGuard {
    fn drop(&mut self) {
        let reg = self.registry.clone();
        let ssid = self.ssid.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                reg.lock().await.remove(&ssid);
            });
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
}
