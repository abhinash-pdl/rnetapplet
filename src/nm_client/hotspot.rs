use std::collections::HashMap;

use anyhow::{Context, Result};
use zbus::zvariant::Value;

use super::NmClient;
use super::uncached_proxy;

pub const HOTSPOT_SSID: &str = "RnetHotspot";

pub const HOTSPOT_CONN_ID: &str = "rnet-hotspot";

pub fn validate_hotspot(ssid: &str, psk: &str) -> Result<(), String> {
    let ssid = ssid.trim();
    if ssid.is_empty() {
        return Err("Enter a hotspot name".into());
    }
    if ssid.len() > 32 {
        return Err("Hotspot name is too long (32-byte limit)".into());
    }
    if psk.len() < 8 {
        return Err("Password needs at least 8 characters".into());
    }
    if psk.len() > 63 {
        return Err("Password must be 63 characters or fewer".into());
    }
    if !psk.is_ascii() {
        return Err("Password must use ASCII characters".into());
    }
    Ok(())
}

pub fn random_psk() -> std::io::Result<String> {
    const ALPH: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    const DRAW: usize = 248 - 248 % ALPH.len();
    use std::io::Read;
    let mut source = std::fs::File::open("/dev/urandom")?;
    let mut out = String::with_capacity(12);
    let mut buf = [0u8; 64];
    while out.len() < 12 {
        source.read_exact(&mut buf)?;
        for b in buf {
            if (b as usize) < DRAW {
                out.push(ALPH[(b % ALPH.len() as u8) as usize] as char);
                if out.len() == 12 {
                    break;
                }
            }
        }
    }
    Ok(out)
}

fn hotspot_profile<'a>(
    ssid: &'a str,
    psk: &'a str,
) -> HashMap<&'a str, HashMap<&'a str, Value<'a>>> {
    let mut outer: HashMap<&str, HashMap<&str, Value>> = HashMap::new();

    let mut connection = HashMap::new();
    connection.insert("id", Value::new(HOTSPOT_CONN_ID));
    connection.insert("type", Value::new("802-11-wireless"));
    connection.insert("autoconnect", Value::new(false));
    outer.insert("connection", connection);

    let mut wireless = HashMap::new();
    wireless.insert("ssid", Value::new(ssid.as_bytes()));
    wireless.insert("mode", Value::new("ap"));
    wireless.insert("band", Value::new("bg"));
    outer.insert("802-11-wireless", wireless);

    let mut sec = HashMap::new();
    sec.insert("key-mgmt", Value::new("wpa-psk"));
    sec.insert("proto", Value::new(vec!["rsn"]));
    sec.insert("pairwise", Value::new(vec!["ccmp"]));
    sec.insert("group", Value::new(vec!["ccmp"]));
    sec.insert("psk", Value::new(psk));
    outer.insert("802-11-wireless-security", sec);

    let mut ipv4 = HashMap::new();
    ipv4.insert("method", Value::new("shared"));
    outer.insert("ipv4", ipv4);

    let mut ipv6 = HashMap::new();
    ipv6.insert("method", Value::new("ignore"));
    outer.insert("ipv6", ipv6);

    outer
}

#[derive(Clone)]
pub struct HotspotPrev {
    pub profile: zbus::zvariant::OwnedObjectPath,
    pub ssid: String,
    pub device: zbus::zvariant::OwnedObjectPath,
}

#[derive(Clone)]
pub struct HotspotStart {
    pub ssid: String,
    pub psk: String,
    pub shared_radio: bool,
    pub prev: Option<HotspotPrev>,
}

impl NmClient {
    pub async fn hotspot_profile_path(&self) -> Result<Option<zbus::zvariant::OwnedObjectPath>> {
        super::profiles::hotspot_profile_path(self.system_conn()).await
    }

    pub async fn saved_hotspot_config(&self) -> Result<Option<(String, Option<String>)>> {
        let Some(path) = self.hotspot_profile_path().await? else {
            return Ok(None);
        };
        let map = super::get_settings(self.system_conn(), &path)
            .await
            .ok_or_else(|| anyhow::anyhow!("could not read hotspot profile"))?;
        let ssid = super::profiles::wireless_ssid(&map);
        let psk = self.hotspot_secret(&path).await;
        Ok(ssid.map(|s| (s, psk)))
    }

    async fn hotspot_secret(&self, profile: &zbus::zvariant::OwnedObjectPath) -> Option<String> {
        use rusty_network_manager::SettingsConnectionProxy;
        let proxy =
            super::uncached_proxy!(SettingsConnectionProxy, self.system_conn(), profile.clone())?;
        let secrets = proxy.get_secrets("802-11-wireless-security").await.ok()?;
        secrets
            .get("802-11-wireless-security")
            .and_then(|sec| sec.get("psk"))
            .and_then(|v| match &**v {
                zbus::zvariant::Value::Str(s) => Some(s.to_string()),
                _ => None,
            })
    }

    async fn update_hotspot_profile(
        &self,
        path: &zbus::zvariant::OwnedObjectPath,
        ssid: &str,
        psk: &str,
    ) -> Result<()> {
        let Some(proxy) = super::uncached_proxy!(
            rusty_network_manager::SettingsConnectionProxy,
            self.system_conn(),
            path.clone()
        ) else {
            anyhow::bail!("could not open the hotspot profile");
        };
        let mut map = super::get_settings(self.system_conn(), path)
            .await
            .context("could not read the hotspot profile")?;
        if !map.contains_key("connection") {
            anyhow::bail!("hotspot profile has no connection group; refusing to rewrite it");
        }
        let wireless = map.entry("802-11-wireless".to_string()).or_default();
        wireless.remove("ap-interface-name");
        wireless.insert(
            "ssid".to_string(),
            zbus::zvariant::OwnedValue::try_from(Value::new(ssid.as_bytes()))
                .context("could not encode the hotspot name")?,
        );
        wireless.insert(
            "mode".to_string(),
            zbus::zvariant::OwnedValue::try_from(Value::new("ap")).expect("static str encodes"),
        );
        wireless.insert(
            "band".to_string(),
            zbus::zvariant::OwnedValue::try_from(Value::new("bg")).expect("static str encodes"),
        );
        let sec = map
            .entry("802-11-wireless-security".to_string())
            .or_default();
        sec.insert(
            "key-mgmt".to_string(),
            zbus::zvariant::OwnedValue::try_from(Value::new("wpa-psk"))
                .expect("static str encodes"),
        );
        sec.insert(
            "proto".to_string(),
            zbus::zvariant::OwnedValue::try_from(Value::new(vec!["rsn"]))
                .expect("static strs encode"),
        );
        sec.insert(
            "pairwise".to_string(),
            zbus::zvariant::OwnedValue::try_from(Value::new(vec!["ccmp"]))
                .expect("static strs encode"),
        );
        sec.insert(
            "group".to_string(),
            zbus::zvariant::OwnedValue::try_from(Value::new(vec!["ccmp"]))
                .expect("static strs encode"),
        );
        sec.insert(
            "psk".to_string(),
            zbus::zvariant::OwnedValue::try_from(Value::new(psk))
                .context("could not encode the hotspot password")?,
        );
        let mut full: HashMap<&str, HashMap<&str, Value>> = HashMap::new();
        for (group, values) in &map {
            let mut row: HashMap<&str, Value> = HashMap::new();
            for (key, value) in values {
                row.insert(
                    key.as_str(),
                    Value::try_from(value).context("could not re-encode the hotspot profile")?,
                );
            }
            full.insert(group.as_str(), row);
        }
        proxy.update(full).await?;
        tracing::info!(%path, "hotspot profile updated in place");
        Ok(())
    }
    async fn launch_hotspot(
        &self,
        ssid: &str,
        psk: &str,
        wifi: &zbus::zvariant::OwnedObjectPath,
        root: &zbus::zvariant::OwnedObjectPath,
        window: std::time::Duration,
    ) -> Result<()> {
        match self.hotspot_profile_path().await {
            Ok(Some(path)) => {
                self.update_hotspot_profile(&path, ssid, psk).await?;
                let active = self.nm.activate_connection(&path, wifi, root).await?;
                tracing::info!(ssid, %path, %active, "hotspot profile reactivated");
                if self
                    .poll_active_state(&active, &crate::state::Ssid::from(ssid), window)
                    .await
                {
                    Ok(())
                } else {
                    anyhow::bail!(
                        "hotspot did not come up; check AP support and whether dnsmasq is installed"
                    );
                }
            }
            _ => {
                let profile = hotspot_profile(ssid, psk);
                let (conn, active) = self
                    .nm
                    .add_and_activate_connection(profile, wifi, root)
                    .await?;
                tracing::info!(ssid, %conn, %active, "hotspot activation started");
                if self
                    .poll_active_state(&active, &crate::state::Ssid::from(ssid), window)
                    .await
                {
                    super::profiles::delete_all_hotspot_profiles_except(self.system_conn(), &conn)
                        .await;
                    Ok(())
                } else {
                    let _ = self.delete_profile(&conn).await;
                    anyhow::bail!(
                        "hotspot did not come up; check AP support and whether dnsmasq is installed"
                    );
                }
            }
        }
    }

    async fn hotspot_device_choice(&self) -> Result<(zbus::zvariant::OwnedObjectPath, bool)> {
        use rusty_network_manager::ActiveProxy;
        let mut devices = Vec::new();
        for path in self.nm.get_devices().await.unwrap_or_default() {
            let Some(dev) = super::uncached_proxy!(
                rusty_network_manager::DeviceProxy,
                self.system_conn(),
                path.clone()
            ) else {
                continue;
            };
            if dev.device_type().await.unwrap_or(0) == super::NM_DEVICE_TYPE_WIFI {
                devices.push(path);
            }
        }
        if devices.is_empty() {
            anyhow::bail!("No Wi-Fi adapter found");
        }
        let mut sta_device: Option<zbus::zvariant::OwnedObjectPath> = None;
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Some(active) = super::uncached_proxy!(ActiveProxy, self.system_conn(), path) else {
                continue;
            };
            if active.type_().await.as_deref() != Ok("802-11-wireless") {
                continue;
            }
            if active.state().await.unwrap_or(0) != 2 {
                continue;
            }
            let Ok(profile) = active.connection().await else {
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
            if let Ok(devs) = active.devices().await
                && let Some(dev) = devs.into_iter().find(|d| devices.contains(d))
            {
                sta_device = Some(dev);
                break;
            }
        }
        if let Some(sta) = sta_device {
            if let Some(free) = devices.iter().find(|d| *d != &sta) {
                return Ok((free.clone(), false));
            }
            return Ok((sta, true));
        }
        let Some(first) = devices.into_iter().next() else {
            anyhow::bail!("No Wi-Fi adapter found");
        };
        Ok((first, false))
    }

    pub async fn create_hotspot_with(&self, ssid: &str, psk: &str) -> Result<HotspotStart> {
        const WIFI_CAP_AP: u32 = 0x40;
        const HOTSPOT_LAND_SECS: u64 = 20;
        let ssid = ssid.trim().to_string();
        validate_hotspot(&ssid, psk).map_err(|e| anyhow::anyhow!(e))?;
        if !self.nm.wireless_enabled().await.unwrap_or(true) {
            anyhow::bail!("Wi-Fi is turned off");
        }
        let (wifi, shared_radio) = self.hotspot_device_choice().await?;
        let mut ap_supported = false;
        if let Some(w) = super::uncached_proxy!(
            rusty_network_manager::WirelessProxy,
            self.system_conn(),
            wifi.clone()
        ) && let Ok(caps) = w.wireless_capabilities().await
        {
            ap_supported = caps & WIFI_CAP_AP != 0;
        }
        if !ap_supported {
            anyhow::bail!("this Wi-Fi adapter cannot host a hotspot");
        }
        let prev = if shared_radio {
            self.active_wifi_snapshot()
                .await
                .map(|(profile, ssid)| HotspotPrev {
                    profile,
                    ssid,
                    device: wifi.clone(),
                })
        } else {
            None
        };
        let root = super::connect::root_path()?;
        let window = std::time::Duration::from_secs(HOTSPOT_LAND_SECS);
        self.launch_hotspot(&ssid, psk, &wifi, &root, window)
            .await?;
        Ok(HotspotStart {
            ssid,
            psk: psk.to_string(),
            shared_radio,
            prev,
        })
    }

    pub async fn hotspot_is_up(&self) -> bool {
        super::active_hotspot_profile(self.system_conn())
            .await
            .ok()
            .flatten()
            .is_some()
    }

    pub async fn describe_wifi_actives(&self) -> Vec<String> {
        use zbus::fdo::PropertiesProxy;
        let mut out = Vec::new();
        let Ok(nm) = rusty_network_manager::NetworkManagerProxy::new(self.system_conn()).await
        else {
            return vec!["nm-proxy-failed".to_string()];
        };
        let actives = nm.active_connections().await.unwrap_or_default();
        out.push(format!("n_actives={}", actives.len()));
        let iface = zbus::names::InterfaceName::try_from(
            "org.freedesktop.NetworkManager.Connection.Active",
        )
        .expect("static interface name parses");
        for path in actives {
            let entry = async {
                let proxy = PropertiesProxy::builder(self.system_conn())
                    .destination("org.freedesktop.NetworkManager")
                    .ok()?
                    .path(path.clone())
                    .ok()?
                    .cache_properties(zbus::proxy::CacheProperties::No)
                    .build()
                    .await
                    .ok()?;
                let all = proxy.get_all(iface.clone()).await.ok()?;
                let str_of = |k: &str| -> String {
                    all.get(k)
                        .and_then(|v| match &**v {
                            zbus::zvariant::Value::Str(s) => Some(s.to_string()),
                            _ => None,
                        })
                        .unwrap_or_else(|| "?".to_string())
                };
                let state = all
                    .get("State")
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or(999);
                Some(format!("{}:{}:{}", str_of("Id"), str_of("Type"), state))
            }
            .await
            .unwrap_or_else(|| "unreadable".to_string());
            out.push(entry);
        }
        out
    }

    pub async fn wait_for_wifi_device(
        &self,
        timeout: std::time::Duration,
    ) -> Result<zbus::zvariant::OwnedObjectPath> {
        let start = std::time::Instant::now();
        loop {
            if let Ok(Some(path)) = self.wifi_device_path().await {
                return Ok(path);
            }
            if start.elapsed() >= timeout {
                anyhow::bail!("Wi-Fi adapter not found");
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }

    pub async fn reactivate_hotspot_prev(&self, prev: &HotspotPrev) -> Result<()> {
        if self.wifi_device_path().await.ok().flatten().is_none() {
            tracing::info!("wifi device gone; waiting for it to return before restoring station");
            self.wait_for_wifi_device(std::time::Duration::from_secs(20))
                .await?;
        }
        let root = super::connect::root_path()?;
        self.nm
            .activate_connection(&prev.profile, &prev.device, &root)
            .await?;
        tracing::info!(ssid = %prev.ssid, "station connection reactivated after hotspot");
        Ok(())
    }

    pub async fn create_hotspot(&self) -> Result<HotspotStart> {
        match self.saved_hotspot_config().await {
            Ok(Some((ssid, Some(psk)))) => self.create_hotspot_with(&ssid, &psk).await,
            Ok(Some((ssid, None))) => {
                tracing::info!(ssid, "saved hotspot password unreadable; starting fresh");
                let psk = random_psk()?;
                self.create_hotspot_with(&ssid, &psk).await
            }
            _ => {
                let psk = random_psk()?;
                self.create_hotspot_with(HOTSPOT_SSID, &psk).await
            }
        }
    }

    pub async fn stop_hotspot(&self) -> Result<()> {
        use rusty_network_manager::ActiveProxy;
        let mut stopped = 0u32;
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Some(active) = uncached_proxy!(ActiveProxy, self.system_conn(), path.clone())
            else {
                continue;
            };
            if active.type_().await.as_deref() != Ok("802-11-wireless") {
                continue;
            }
            let id = active.id().await.unwrap_or_default();
            let is_hs = super::active_conn_is_hotspot(self.system_conn(), &path).await
                || id == HOTSPOT_CONN_ID
                || id == HOTSPOT_SSID;
            if is_hs && self.nm.deactivate_connection(&path).await.is_ok() {
                tracing::info!(%path, "hotspot deactivated");
                stopped += 1;
            }
        }
        if stopped > 0 {
            Ok(())
        } else {
            anyhow::bail!("no active hotspot to stop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psk_charset_and_len() {
        let p = random_psk().unwrap();
        assert_eq!(p.len(), 12);
        assert!(
            p.bytes()
                .all(|b| b"abcdefghjkmnpqrstuvwxyz23456789".contains(&b))
        );
    }

    #[test]
    fn hotspot_validation() {
        assert!(validate_hotspot("MyHotspot", "12345678").is_ok());
        assert!(validate_hotspot("", "12345678").is_err());
        assert!(validate_hotspot("H", "short").is_err());
        assert!(validate_hotspot("H", "longenough123").is_ok());
    }

    #[test]
    fn hotspot_profile_shape() {
        let p = hotspot_profile("RnetHotspot", "secret12345");
        assert!(p.contains_key("802-11-wireless-security"));
        assert!(matches!(&p["802-11-wireless"]["mode"], Value::Str(s) if s.as_str() == "ap"));
        assert!(matches!(&p["ipv4"]["method"], Value::Str(s) if s.as_str() == "shared"));
        assert!(matches!(
            &p["connection"]["autoconnect"],
            Value::Bool(false)
        ));
    }
}
