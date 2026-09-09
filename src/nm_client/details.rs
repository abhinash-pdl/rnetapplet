use anyhow::Result;
use zbus::zvariant::OwnedObjectPath;

use super::NmClient;
use crate::state::HotspotInfo;

pub async fn active_details(
    client: &NmClient,
) -> (Option<String>, Option<String>, Option<u32>) {
    let Some(wifi) = client.wifi_device_path().await.unwrap_or(None) else {
        return (None, None, None);
    };
    let Ok(dev) =
        rusty_network_manager::DeviceProxy::new_from_path(wifi.clone(), client.system_conn())
            .await
    else {
        return (None, None, None);
    };
    let iface = dev.interface().await.ok();

    let ip_path = dev.ip4_config().await.ok().filter(|p| p.as_str() != "/");
    let mut ipv4 = None;
    if let Some(p) = ip_path {
        ipv4 = read_first_address(client, &p).await;
    }

    let mut bitrate = None;
    if let Ok(w) =
        rusty_network_manager::WirelessProxy::new_from_path(wifi, client.system_conn()).await
    {
        bitrate = w.bitrate().await.ok().filter(|b| *b > 0);
    }

    (iface, ipv4, bitrate)
}

async fn read_first_address(client: &NmClient, path: &OwnedObjectPath) -> Option<String> {
    let proxy =
        rusty_network_manager::IP4ConfigProxy::new_from_path(path.clone(), client.system_conn())
            .await
            .ok()?;
    let data = proxy.address_data().await.ok()?;
    let first = data.first()?;
    match &**first.get("address")? {
        zbus::zvariant::Value::Str(s) => Some(s.to_string()),
        _ => None,
    }
}

pub async fn hotspot_status(client: &NmClient) -> Result<Option<HotspotInfo>> {
    use rusty_network_manager::{ActiveProxy, SettingsConnectionProxy};
    for path in client.nm.active_connections().await.unwrap_or_default() {
        let Ok(active) = ActiveProxy::new_from_path(path, client.system_conn()).await else {
            continue;
        };
        if active.type_().await.as_deref() != Ok("802-11-wireless") {
            continue;
        }

        if active.state().await.unwrap_or(0) != 2 {
            continue;
        }
        let Ok(conn_path) = active.connection().await else {
            continue;
        };
        let Ok(sc) =
            SettingsConnectionProxy::new_from_path(conn_path, client.system_conn()).await
        else {
            continue;
        };
        let Ok(map) = sc.get_settings().await else {
            continue;
        };
        let is_ap = map
            .get("802-11-wireless")
            .and_then(|w| w.get("mode"))
            .map(|v| matches!(&**v, zbus::zvariant::Value::Str(s) if s.as_str() == "ap"))
            .unwrap_or(false);
        if !is_ap {
            continue;
        }
        let from_map = map
            .get("802-11-wireless")
            .and_then(|w| w.get("ssid"))
            .and_then(|v| match &**v {
                zbus::zvariant::Value::Array(arr) => {
                    let bytes: Vec<u8> =
                        arr.iter().filter_map(|b| u8::try_from(b).ok()).collect();
                    super::aps::decode_ssid(&bytes)
                }
                _ => None,
            });
        let ssid = match from_map {
            Some(s) => s,
            None => active.id().await.unwrap_or_else(|_| "hotspot".into()),
        };
        let psk = map
            .get("802-11-wireless-security")
            .and_then(|w| w.get("psk"))
            .and_then(|v| match &**v {
                zbus::zvariant::Value::Str(s) => Some(s.to_string()),
                _ => None,
            });
        return Ok(Some(HotspotInfo {
            ssid,
            psk,
            active: true,
        }));
    }

    if let Ok(Some((ssid, psk))) = client.saved_hotspot_config().await {
        return Ok(Some(HotspotInfo {
            ssid,
            psk,
            active: false,
        }));
    }
    Ok(None)
}
