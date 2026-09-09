use std::collections::HashMap;

use anyhow::{Context, Result};
use zbus::zvariant::Value;

use super::NmClient;

pub const HOTSPOT_SSID: &str = "RnetHotspot";

pub const HOTSPOT_CONN_ID: &str = "rnet-hotspot";

pub fn validate_hotspot(ssid: &str, psk: &str) -> Result<(), String> {
    let ssid = ssid.trim();
    if ssid.is_empty() {
        return Err("Enter a hotspot name".into());
    }
    if ssid.len() > 32 {
        return Err("Hotspot name must be 32 characters or fewer".into());
    }
    if psk.len() < 8 {
        return Err("Password needs at least 8 characters".into());
    }
    if psk.len() > 63 {
        return Err("Password must be 63 characters or fewer".into());
    }
    Ok(())
}

pub fn random_psk() -> String {
    const ALPH: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut buf = [0u8; 12];
    let ok = (|| -> std::io::Result<()> {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)
    })()
    .is_ok();
    if !ok {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        use std::time::SystemTime;
        let mut hasher = DefaultHasher::new();
        SystemTime::now().hash(&mut hasher);
        std::process::id().hash(&mut hasher);

        buf = hasher.finish().to_le_bytes().repeat(2)[..12]
            .try_into()
            .unwrap_or([b'x'; 12]);
        tracing::warn!("/dev/urandom unreadable, using hashed fallback PSK");
    }
    buf.iter()
        .map(|b| ALPH[(b % ALPH.len() as u8) as usize] as char)
        .collect()
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
    outer.insert("802-11-wireless", wireless);

    let mut sec = HashMap::new();
    sec.insert("key-mgmt", Value::new("wpa-psk"));
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

impl NmClient {

    pub async fn hotspot_profile_path(
        &self,
    ) -> Result<Option<zbus::zvariant::OwnedObjectPath>> {
        use rusty_network_manager::{SettingsConnectionProxy, SettingsProxy};
        let settings = SettingsProxy::new(self.system_conn()).await?;
        for path in settings.list_connections().await.unwrap_or_default() {
            let Ok(proxy) =
                SettingsConnectionProxy::new_from_path(path.clone(), self.system_conn()).await
            else {
                continue;
            };
            let Ok(map) = proxy.get_settings().await else {
                continue;
            };
            let is_ap = map
                .get("802-11-wireless")
                .and_then(|w| w.get("mode"))
                .map(|v| matches!(&**v, Value::Str(s) if s.as_str() == "ap"))
                .unwrap_or(false);
            if is_ap {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    pub async fn saved_hotspot_config(&self) -> Result<Option<(String, Option<String>)>> {
        use rusty_network_manager::SettingsConnectionProxy;
        let Some(path) = self.hotspot_profile_path().await? else {
            return Ok(None);
        };
        let proxy =
            SettingsConnectionProxy::new_from_path(path, self.system_conn()).await?;
        let map = proxy.get_settings().await?;
        let ssid = map
            .get("802-11-wireless")
            .and_then(|w| w.get("ssid"))
            .and_then(|v| match &**v {
                Value::Array(arr) => {
                    let bytes: Vec<u8> =
                        arr.iter().filter_map(|b| u8::try_from(b).ok()).collect();
                    crate::nm_client::aps::decode_ssid(&bytes)
                }
                _ => None,
            });
        let psk = map
            .get("802-11-wireless-security")
            .and_then(|w| w.get("psk"))
            .and_then(|v| match &**v {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            });
        Ok(ssid.map(|s| (s, psk)))
    }

    pub async fn create_hotspot_with(&self, ssid: &str, psk: &str) -> Result<(String, String)> {
        let ssid = ssid.trim().to_string();
        validate_hotspot(&ssid, psk).map_err(|e| anyhow::anyhow!(e))?;
        let wifi = self
            .wifi_device_path()
            .await?
            .context("no Wi-Fi device found")?;
        let root = super::connect::root_path()?;
        if let Ok(Some((saved_ssid, saved_psk))) = self.saved_hotspot_config().await {
            if saved_ssid == ssid && saved_psk.as_deref() == Some(psk) {
                if let Ok(Some(path)) = self.hotspot_profile_path().await {
                    let active = self.nm.activate_connection(&path, &wifi, &root).await?;
                    tracing::info!(%active, ssid, "saved hotspot activated");
                    return Ok((ssid, psk.to_string()));
                }
            }
        }
        use rusty_network_manager::{SettingsConnectionProxy, SettingsProxy};
        if let Ok(settings) = SettingsProxy::new(self.system_conn()).await {
            for path in settings.list_connections().await.unwrap_or_default() {
                let Ok(proxy) =
                    SettingsConnectionProxy::new_from_path(path.clone(), self.system_conn())
                        .await
                else {
                    continue;
                };
                let Ok(map) = proxy.get_settings().await else {
                    continue;
                };
                let is_ap = map
                    .get("802-11-wireless")
                    .and_then(|w| w.get("mode"))
                    .map(|v| matches!(&**v, Value::Str(s) if s.as_str() == "ap"))
                    .unwrap_or(false);
                if is_ap {
                    let _ = proxy.delete().await;
                }
            }
        }
        let profile = hotspot_profile(&ssid, psk);
        let (conn, active) = self
            .nm
            .add_and_activate_connection(profile, &wifi, &root)
            .await?;
        tracing::info!(%conn, %active, ssid, "hotspot activated");
        Ok((ssid, psk.to_string()))
    }

    pub async fn create_hotspot(&self) -> Result<(String, String)> {

        if let Ok(Some((ssid, Some(psk)))) = self.saved_hotspot_config().await {
            return self.create_hotspot_with(&ssid, &psk).await;
        }
        let psk = random_psk();
        self.create_hotspot_with(HOTSPOT_SSID, &psk).await
    }

    pub async fn stop_hotspot(&self) -> Result<()> {
        use rusty_network_manager::{ActiveProxy, SettingsConnectionProxy};
        for path in self.nm.active_connections().await.unwrap_or_default() {
            let Ok(active) = ActiveProxy::new_from_path(path.clone(), self.system_conn()).await
            else {
                continue;
            };

            let mut is_hs = false;
            if let Ok(conn_path) = active.connection().await {
                if let Ok(sc) =
                    SettingsConnectionProxy::new_from_path(conn_path, self.system_conn()).await
                {
                    if let Ok(map) = sc.get_settings().await {
                        is_hs = map
                            .get("802-11-wireless")
                            .and_then(|w| w.get("mode"))
                            .map(|v| {
                                matches!(&**v, Value::Str(s) if s.as_str() == "ap")
                            })
                            .unwrap_or(false);
                    }
                }
            }

            let id_match = active.id().await.as_deref() == Ok(HOTSPOT_CONN_ID)
                || active.id().await.as_deref() == Ok(HOTSPOT_SSID);
            if is_hs || id_match {
                self.nm.deactivate_connection(&path).await?;
                tracing::info!("hotspot deactivated");
                return Ok(());
            }
        }
        anyhow::bail!("no active hotspot to stop");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psk_charset_and_len() {
        let p = random_psk();
        assert_eq!(p.len(), 12);
        assert!(p.bytes().all(|b| b"abcdefghjkmnpqrstuvwxyz23456789".contains(&b)));
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
        assert!(matches!(&p["connection"]["autoconnect"], Value::Bool(false)));
    }
}
