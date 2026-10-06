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
    pub async fn connect_open(&self, ssid: &str) -> Result<Option<OwnedObjectPath>> {
        self.disconnect_other_wifi(ssid).await?;
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let ap = self.ap_path_for_ssid(&wifi, ssid).await?;
        let existing = super::saved_profile_path(self.system_conn(), ssid).await?;
        let mut created = None;
        let active = if let Some(profile) = existing {
            self.nm.activate_connection(&profile, &wifi, &ap).await?
        } else {
            let profile = open_profile(ssid);
            let (conn, active) = self
                .nm
                .add_and_activate_connection(profile, &wifi, &ap)
                .await
                .inspect_err(|e| tracing::warn!(ssid, "AddAndActivateConnection failed: {e:#}"))?;
            tracing::info!(ssid, %conn, "AddAndActivateConnection issued (open, new profile)");
            created = Some(conn);
            active
        };
        tracing::info!(ssid, %active, "ActivateConnection issued (open)");
        self.drop_profile_cache().await;
        Ok(created)
    }
    pub async fn discard_new_profile(&self, ssid: &str) {
        if let Ok(Some(path)) = super::saved_profile_path(self.system_conn(), ssid).await
            && let Ok(proxy) = rusty_network_manager::SettingsConnectionProxy::new_from_path(
                path,
                self.system_conn(),
            )
            .await
        {
            match proxy.delete().await {
                Ok(()) => tracing::info!(ssid, "discarded new profile after failed connect"),
                Err(e) => tracing::warn!(ssid, "could not discard new profile: {e}"),
            }
        }
        self.drop_profile_cache().await;
    }
    pub async fn connect_secure(&self, ssid: &str, psk: &str) -> Result<Option<OwnedObjectPath>> {
        validate_psk(psk)?;
        self.disconnect_other_wifi(ssid).await?;
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let ap = self.ap_path_for_ssid(&wifi, ssid).await?;
        let existing = super::saved_profile_path(self.system_conn(), ssid).await?;
        let mut created = None;
        let active = if let Some(profile) = existing {
            self.update_psk(&profile, psk).await?;
            self.nm.activate_connection(&profile, &wifi, &ap).await?
        } else {
            let profile = secure_profile(ssid, psk);
            match self
                .nm
                .add_and_activate_connection(profile, &wifi, &ap)
                .await
            {
                Ok((conn, active)) => {
                    tracing::info!(ssid, %conn, "AddAndActivateConnection issued (secure, new profile)");
                    created = Some(conn);
                    active
                }
                Err(e) => {
                    self.discard_new_profile(ssid).await;
                    return Err(anyhow::Error::from(e));
                }
            }
        };
        tracing::info!(ssid, %active, "ActivateConnection issued (secure)");
        self.drop_profile_cache().await;
        Ok(created)
    }
    pub async fn connect_hidden(&self, ssid: &str, psk: &str) -> Result<()> {
        if !psk.is_empty() {
            validate_psk(psk)?;
        }
        self.disconnect_other_wifi(ssid).await?;
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
        self.drop_profile_cache().await;
        Ok(())
    }
    pub async fn update_psk(&self, profile: &OwnedObjectPath, psk: &str) -> Result<()> {
        let proxy = rusty_network_manager::SettingsConnectionProxy::new_from_path(
            profile.clone(),
            self.system_conn(),
        )
        .await?;

        let mut map = super::get_settings(self.system_conn(), profile)
            .await
            .context("could not read the saved profile")?;
        if !map.contains_key("connection") {
            anyhow::bail!("saved profile has no connection group; refusing to rewrite it");
        }
        let new_secret = zbus::zvariant::OwnedValue::try_from(Value::new(psk))
            .context("could not encode the new password")?;
        map.entry("802-11-wireless-security".to_string())
            .or_default()
            .insert("psk".to_string(), new_secret);
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
        self.drop_profile_cache().await;
        Ok(())
    }
    pub async fn connect_saved(&self, ssid: &str) -> Result<Option<OwnedObjectPath>> {
        self.disconnect_other_wifi(ssid).await?;
        let profile = super::saved_profile_path(self.system_conn(), ssid)
            .await?
            .with_context(|| format!("no saved profile for {ssid}"))?;
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let ap = self.ap_path_for_ssid(&wifi, ssid).await?;
        tracing::info!(ssid, %profile, %wifi, %ap, "activating saved profile");
        let active = self
            .nm
            .activate_connection(&profile, &wifi, &ap)
            .await
            .with_context(|| format!("ActivateConnection failed for saved {ssid}"))?;
        tracing::info!(ssid, %active, "ActivateConnection issued (saved)");
        Ok(None)
    }

    pub async fn retry_saved_with_psk(&self, ssid: &str, psk: &str) -> Result<SavedRetry> {
        validate_psk(psk)?;
        self.disconnect_other_wifi(ssid).await?;
        let profile = super::saved_profile_path(self.system_conn(), ssid)
            .await?
            .with_context(|| format!("no saved profile for {ssid}"))?;
        let old_psk =
            super::profiles::load_wireless_settings(self.system_conn(), vec![profile.clone()])
                .await
                .into_iter()
                .next()
                .and_then(|(_, map)| super::profiles::profile_psk(&map));
        let rollback = old_psk.clone().map(|old| (profile.clone(), old));
        tracing::info!(
            ssid,
            %profile,
            new_len = psk.len(),
            old_len = rollback.as_ref().map(|(_, p)| p.len()),
            "saved profile selected; writing the new password"
        );
        self.update_psk(&profile, psk).await?;
        match self.connect_saved(ssid).await {
            Ok(created) => Ok(SavedRetry { created, rollback }),
            Err(e) => {
                tracing::warn!(
                    ssid,
                    "first activation after password change failed: {e:#}; retrying once"
                );
                tokio::time::sleep(std::time::Duration::from_millis(900)).await;
                match self.connect_saved(ssid).await {
                    Ok(created) => Ok(SavedRetry { created, rollback }),
                    Err(e) => {
                        if let Some((profile, old)) = &rollback {
                            let _ = self.update_psk(profile, old).await;
                        }
                        Err(e)
                    }
                }
            }
        }
    }

    pub async fn connect_saved_with_psk(
        &self,
        ssid: &str,
        psk: &str,
    ) -> Result<Option<OwnedObjectPath>> {
        validate_psk(psk)?;
        let profile = super::saved_profile_path(self.system_conn(), ssid)
            .await?
            .with_context(|| format!("no saved profile for {ssid}"))?;
        self.update_psk(&profile, psk).await?;
        self.connect_saved(ssid).await
    }

    pub async fn disconnect_other_wifi(&self, keep_ssid: &str) -> Result<bool> {
        use rusty_network_manager::ActiveProxy;
        let mut dropped = false;
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Ok(proxy) = ActiveProxy::new_from_path(path.clone(), self.system_conn()).await
            else {
                continue;
            };
            if proxy.type_().await.as_deref() != Ok("802-11-wireless")
                || proxy.state().await.unwrap_or(0) != 2
            {
                continue;
            }
            if super::active_conn_is_hotspot(self.system_conn(), &path).await {
                continue;
            }
            let Ok(profile) = proxy.connection().await else {
                continue;
            };
            let Some(map) = super::get_settings(self.system_conn(), &profile).await else {
                continue;
            };
            match super::profiles::wireless_ssid(&map) {
                Some(ssid) if ssid == keep_ssid => continue,
                None => continue,
                Some(_) => {}
            }
            match self.nm.deactivate_connection(&path).await {
                Ok(()) => {
                    tracing::info!(%path, "stepping aside from the active network");
                    dropped = true;
                }
                Err(e) => tracing::warn!(%path, "deactivate before switch failed: {e:#}"),
            }
        }
        if dropped {
            tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        }
        Ok(dropped)
    }

    pub async fn disconnect_active(&self) -> Result<()> {
        use rusty_network_manager::ActiveProxy;
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Ok(proxy) = ActiveProxy::new_from_path(path.clone(), self.system_conn()).await
            else {
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
            let Ok(proxy) = ActiveProxy::new_from_path(path.clone(), self.system_conn()).await
            else {
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
#[allow(clippy::too_many_arguments)]
pub struct SavedRetry {
    pub created: Option<OwnedObjectPath>,
    pub rollback: Option<(OwnedObjectPath, String)>,
}

pub const MSG_FAILED: &str = "Couldn't connect";
pub const MSG_WRONG_PASSWORD: &str = "Incorrect password";

pub struct GuardOpts {
    pub target_ssid: String,
    pub prev: Option<PrevWifi>,
    pub window: Duration,
    pub registry: Arc<tokio::sync::Mutex<std::collections::HashMap<String, u64>>>,
    pub created: Option<OwnedObjectPath>,
    pub manual: Arc<std::sync::atomic::AtomicBool>,

    pub rollback_psk: Option<(OwnedObjectPath, String)>,
}

pub fn spawn_restore_guard(
    client: Arc<NmClient>,
    mut watch_rx: ModelRx,
    feed: async_channel::Sender<crate::state::UiEvent>,
    opts: GuardOpts,
) {
    let GuardOpts {
        target_ssid,
        prev,
        window,
        registry,
        created,
        manual,
        rollback_psk,
    } = opts;
    tokio::spawn(async move {
        let generation = {
            let mut seen = registry.lock().await;
            let slot = seen.entry(target_ssid.clone()).or_insert(0);
            *slot += 1;
            if *slot > 1 {
                tracing::info!(ssid = %target_ssid, generation = *slot, "newer attempt takes over this network");
            }
            *slot
        };
        let guard = GuardGuard {
            registry: registry.clone(),
            ssid: target_ssid.clone(),
            generation,
        };
        let _ = &guard;
        let deadline = tokio::time::Instant::now() + window;
        let mut none_since: Option<tokio::time::Instant> = None;
        let mut saw_disconnect = false;
        loop {
            if registry.lock().await.get(&target_ssid).copied() != Some(generation) {
                tracing::info!(ssid = %target_ssid, "a newer attempt owns this network; standing down");
                return;
            }
            let mut connected = false;
            {
                let m = watch_rx.borrow();
                match m.active_ssid.as_deref() {
                    Some(s) if s == target_ssid.as_str() => {
                        connected = true;
                    }
                    Some(_) if saw_disconnect => break,
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
            if connected {
                client.mark_connected(&target_ssid).await;
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            let _ = tokio::time::timeout(Duration::from_millis(500), watch_rx.changed()).await;
        }
        if watch_rx.borrow().active_ssid.as_deref() == Some(target_ssid.as_str()) {
            return;
        }
        if let Some((profile, old_psk)) = rollback_psk {
            match client.update_psk(&profile, &old_psk).await {
                Ok(()) => tracing::info!(
                    ssid = %target_ssid,
                    "attempt failed; previous saved password restored"
                ),
                Err(e) => tracing::warn!("could not restore previous password: {e:#}"),
            }
        }
        if let Some(path) = created {
            tracing::info!(ssid = %target_ssid, %path, "connect failed; removing profile we created");
            if let Ok(proxy) = rusty_network_manager::SettingsConnectionProxy::new_from_path(
                path,
                client.system_conn(),
            )
            .await
                && let Err(e) = proxy.delete().await
            {
                tracing::warn!("could not remove failed profile: {e}");
            }
        }
        if manual.load(std::sync::atomic::Ordering::Relaxed) {
            tracing::info!(ssid = %target_ssid, "manual disconnect seen; not restoring previous");
            return;
        }
        match &prev {
            Some(p) => match client.reactivate_previous(&p.ssid, &p.profile).await {
                Ok(()) => {
                    tracing::info!(restored = %p.ssid, "restored previous network");
                    let _ = feed.try_send(crate::state::UiEvent::SsidError {
                        ssid: target_ssid.clone(),
                        message: MSG_FAILED.into(),
                    });
                }
                Err(e) => {
                    tracing::warn!("failed to restore previous network {}: {e:#}", p.ssid);
                    let target = target_ssid.clone();
                    match client.restore_by_priority(&target_ssid).await {
                        Ok(Some(ssid)) => {
                            tracing::info!(restored = %ssid, "restored a network by priority");
                            let _ = feed.try_send(crate::state::UiEvent::SsidError {
                                ssid: target,
                                message: MSG_FAILED.into(),
                            });
                        }
                        _ => {
                            tracing::info!("no saved network left to fall back to");
                            let _ = feed.try_send(crate::state::UiEvent::SsidError {
                                ssid: target,
                                message: MSG_FAILED.into(),
                            });
                        }
                    }
                }
            },
            None => {
                if watch_rx.borrow().active_ssid.is_some() {
                    return;
                }
                match client.restore_by_priority(&target_ssid).await {
                    Ok(Some(ssid)) => {
                        tracing::info!(restored = %ssid, "restored a network by priority");
                        let _ = feed.try_send(crate::state::UiEvent::SsidError {
                            ssid: target_ssid,
                            message: MSG_FAILED.into(),
                        });
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("priority fallback failed: {e:#}"),
                }
            }
        }
    });
}
struct GuardGuard {
    registry: Arc<tokio::sync::Mutex<std::collections::HashMap<String, u64>>>,
    ssid: String,
    generation: u64,
}
impl Drop for GuardGuard {
    fn drop(&mut self) {
        let reg = self.registry.clone();
        let ssid = self.ssid.clone();
        let generation = self.generation;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut map = reg.lock().await;
                if map.get(&ssid).copied() == Some(generation) {
                    map.remove(&ssid);
                }
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
