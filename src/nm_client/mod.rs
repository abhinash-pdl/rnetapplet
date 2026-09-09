pub mod aps;
pub mod connect;
pub mod details;
pub mod hotspot;
pub mod radio;
pub mod signals;

use anyhow::{Context, Result};
use rusty_network_manager::{
    AccessPointProxy, ActiveProxy, DeviceProxy, NetworkManagerProxy, SettingsConnectionProxy,
    SettingsProxy, WirelessProxy,
};
use zbus::Connection;
use zbus::zvariant::OwnedObjectPath;

use crate::state::{Ap, Model, VpnConnection};

pub const NM_DEVICE_TYPE_WIFI: u32 = 2;

pub struct NmClient {
    system: Connection,
    pub nm: NetworkManagerProxy<'static>,
}

impl NmClient {
    pub async fn connect() -> Result<Self> {
        let system = Connection::system()
            .await
            .context("failed to connect to system D-Bus")?;
        let nm = NetworkManagerProxy::new(&system)
            .await
            .context("failed to create NetworkManager proxy")?;
        Ok(Self { system, nm })
    }

    pub fn system_conn(&self) -> &Connection {
        &self.system
    }

    pub async fn version(&self) -> Result<String> {
        Ok(self.nm.version().await?)
    }

    pub async fn wifi_device_path(&self) -> Result<Option<OwnedObjectPath>> {
        let devices = self.nm.get_devices().await?;
        for path in devices {
            let dev = DeviceProxy::new_from_path(path.clone(), self.system_conn()).await?;
            if dev.device_type().await? == NM_DEVICE_TYPE_WIFI {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    pub async fn request_scan(&self, wifi_path: &OwnedObjectPath) -> Result<()> {
        let wifi = WirelessProxy::new_from_path(wifi_path.clone(), self.system_conn()).await?;

        let _ = wifi
            .request_scan(std::collections::HashMap::new())
            .await;
        Ok(())
    }

    pub async fn list_aps(&self, wifi_path: &OwnedObjectPath) -> Result<Vec<Ap>> {
        let wifi = WirelessProxy::new_from_path(wifi_path.clone(), self.system_conn()).await?;
        let ap_paths = wifi.get_all_access_points().await.unwrap_or_default();
        let saved_ssids = self.saved_wireless_ssids().await.unwrap_or_default();

        let mut by_ssid: std::collections::HashMap<String, Ap> = std::collections::HashMap::new();
        for path in ap_paths {
            let Ok(proxy) = AccessPointProxy::new_from_path(path.clone(), self.system_conn()).await
            else {
                continue;
            };
            let Ok(ssid_bytes) = proxy.ssid().await else {
                continue;
            };
            let Some(ssid) = aps::decode_ssid(&ssid_bytes) else {
                continue;
            };
            let strength = proxy.strength().await.unwrap_or(0);
            let wpa = proxy.wpa_flags().await.unwrap_or(0);
            let rsn = proxy.rsn_flags().await.unwrap_or(0);
            let ap = Ap {
                saved: saved_ssids.contains(&ssid),
                ssid: ssid.clone(),
                bssid_path: path.to_string(),
                strength,
                secured: aps::is_secured(wpa, rsn),
            };
            by_ssid
                .entry(ssid)
                .and_modify(|e| {
                    if ap.strength > e.strength {
                        *e = ap.clone();
                    }
                })
                .or_insert(ap);
        }
        let mut aps: Vec<Ap> = by_ssid.into_values().collect();
        aps.sort_by(|a, b| b.strength.cmp(&a.strength));
        Ok(aps)
    }

    pub async fn saved_wireless_ssids(&self) -> Result<std::collections::HashSet<String>> {
        use std::collections::HashSet;
        let settings = SettingsProxy::new(self.system_conn()).await?;
        let conns = settings.list_connections().await.unwrap_or_default();
        let mut out = HashSet::new();
        for path in conns {
            let Ok(proxy) =
                SettingsConnectionProxy::new_from_path(path, self.system_conn()).await
            else {
                continue;
            };
            let Ok(map) = proxy.get_settings().await else {
                continue;
            };

            let is_wifi = map
                .get("connection")
                .and_then(|c| c.get("type"))
                .and_then(|v| {
                    use zbus::zvariant::Value;
                    match &**v {
                        Value::Str(s) => Some(s.as_str() == "802-11-wireless"),
                        _ => None,
                    }
                })
                .unwrap_or(false);
            if !is_wifi {
                continue;
            }
            let connected_once = map
                .get("connection")
                .and_then(|c| c.get("timestamp"))
                .map(|v| {
                    use zbus::zvariant::Value;
                    match &**v {
                        Value::U64(t) => *t > 0,
                        Value::I64(t) => *t > 0,
                        _ => false,
                    }
                })
                .unwrap_or(false);
            if !connected_once {
                continue;
            }
            if let Some(ssid_val) = map
                .get("802-11-wireless")
                .and_then(|w| w.get("ssid"))
            {
                use zbus::zvariant::Value;
                if let Value::Array(arr) = &**ssid_val {
                    let bytes: Vec<u8> = arr
                        .iter()
                        .filter_map(|v| u8::try_from(v).ok())
                        .collect();
                    if let Some(ssid) = aps::decode_ssid(&bytes) {
                        out.insert(ssid);
                    }
                }
            }
        }
        Ok(out)
    }

    pub async fn active_ssid(&self) -> Result<Option<String>> {
        let actives = self.nm.active_connections().await.unwrap_or_default();
        for path in actives {
            let Ok(proxy) = ActiveProxy::new_from_path(path, self.system_conn()).await else {
                continue;
            };

            if proxy.state().await.unwrap_or(0) != 2 {
                continue;
            }
            let Ok(typ) = proxy.type_().await else {
                continue;
            };
            if typ == "802-11-wireless" {
                if let Ok(id) = proxy.id().await {
                    return Ok(Some(id));
                }
            }
        }
        Ok(None)
    }

    pub async fn vpn_connections(&self) -> Result<Vec<VpnConnection>> {
        let settings = SettingsProxy::new(self.system_conn()).await?;
        let conns = settings.list_connections().await.unwrap_or_default();
        let actives = self.nm.active_connections().await.unwrap_or_default();
        let mut active_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for path in actives {
            if let Ok(proxy) = ActiveProxy::new_from_path(path, self.system_conn()).await {
                if let (Ok(typ), Ok(id)) = (proxy.type_().await, proxy.id().await) {
                    if typ == "vpn" {
                        active_ids.insert(id);
                    }
                }
            }
        }
        let mut out = Vec::new();
        for path in conns {
            let Ok(proxy) =
                SettingsConnectionProxy::new_from_path(path.clone(), self.system_conn()).await
            else {
                continue;
            };
            let Ok(map) = proxy.get_settings().await else {
                continue;
            };
            use zbus::zvariant::Value;
            let is_vpn = map
                .get("connection")
                .and_then(|c| c.get("type"))
                .map(|v| matches!(&**v, Value::Str(s) if s.as_str() == "vpn"))
                .unwrap_or(false);
            if !is_vpn {
                continue;
            }
            let id = map
                .get("connection")
                .and_then(|c| c.get("id"))
                .and_then(|v| match &**v {
                    Value::Str(s) => Some(s.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| path.to_string());
            let active = active_ids.contains(&id);
            out.push(VpnConnection {
                id,
                path: path.to_string(),
                active,
            });
        }
        Ok(out)
    }

    pub async fn refresh_model(&self) -> Result<Model> {
        let wifi_enabled = self.nm.wireless_enabled().await.unwrap_or(true);
        let networking_enabled = self.nm.networking_enabled().await.unwrap_or(true);
        let wifi_path = self.wifi_device_path().await?;
        let mut aps = Vec::new();
        if let Some(ref path) = wifi_path {

            let _ = self.request_scan(path).await;
            aps = self.list_aps(path).await.unwrap_or_default();
        }
        let mut active_ssid = self.active_ssid().await.unwrap_or(None);
        let vpn_connections = self.vpn_connections().await.unwrap_or_default();
        let (active_iface, active_ipv4, active_bitrate_kbps) =
            details::active_details(self).await;
        let hotspot = details::hotspot_status(self).await.unwrap_or(None);

        if let Some(hs) = hotspot.as_ref().filter(|h| h.active) {
            aps.retain(|ap| ap.ssid != hs.ssid);

            if active_ssid.as_deref() == Some(hs.ssid.as_str()) {
                active_ssid = None;
            }
        }
        Ok(Model {
            aps,
            active_ssid,
            active_iface,
            active_ipv4,
            active_bitrate_kbps,
            hotspot,
            wifi_enabled,
            networking_enabled,
            nm_online: true,
            vpn_connections,
        })
    }
}
