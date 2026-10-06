use super::NmClient;
use crate::state::{HotspotInfo, WiredInfo};
use anyhow::Result;
use zbus::zvariant::OwnedObjectPath;
pub struct ActiveNet {
    pub iface: Option<String>,
    pub ipv4: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    pub freq_mhz: Option<u32>,
}
pub async fn active_details(client: &NmClient, wifi_path: Option<OwnedObjectPath>) -> ActiveNet {
    let Some(wifi) = wifi_path else {
        return ActiveNet {
            iface: None,
            ipv4: None,
            bitrate_kbps: None,
            gateway: None,
            dns: Vec::new(),
            freq_mhz: None,
        };
    };
    let Ok(dev) =
        rusty_network_manager::DeviceProxy::new_from_path(wifi.clone(), client.system_conn()).await
    else {
        return ActiveNet {
            iface: None,
            ipv4: None,
            bitrate_kbps: None,
            gateway: None,
            dns: Vec::new(),
            freq_mhz: None,
        };
    };
    let iface = dev.interface().await.ok();
    let ip_path = dev.ip4_config().await.ok().filter(|p| p.as_str() != "/");
    let ipv4 = match ip_path.clone() {
        Some(p) => read_first_address(client, &p).await,
        None => None,
    };
    let gateway = match ip_path.clone() {
        Some(p) => read_gateway(client, &p).await,
        None => None,
    };
    let dns = match ip_path {
        Some(p) => read_dns(client, &p).await,
        None => Vec::new(),
    };
    let wireless =
        rusty_network_manager::WirelessProxy::new_from_path(wifi, client.system_conn()).await;
    let mut bitrate = None;
    let mut ap_path = None;
    if let Ok(w) = wireless.as_ref() {
        bitrate = w.bitrate().await.ok().filter(|b| *b > 0);
        ap_path = w.active_access_point().await.ok();
    }
    let mut freq_mhz = None;
    if let Some(ap_path) = ap_path.filter(|p| p.as_str() != "/")
        && let Ok(ap) =
            rusty_network_manager::AccessPointProxy::new_from_path(ap_path, client.system_conn())
                .await
    {
        freq_mhz = ap.frequency().await.ok();
    }
    ActiveNet {
        iface,
        ipv4,
        bitrate_kbps: bitrate,
        gateway,
        dns,
        freq_mhz,
    }
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
async fn read_gateway(client: &NmClient, path: &OwnedObjectPath) -> Option<String> {
    let proxy =
        rusty_network_manager::IP4ConfigProxy::new_from_path(path.clone(), client.system_conn())
            .await
            .ok()?;
    proxy
        .gateway()
        .await
        .ok()
        .filter(|s| !s.is_empty() && s != "0.0.0.0")
}
async fn read_dns(client: &NmClient, path: &OwnedObjectPath) -> Vec<String> {
    let Ok(proxy) =
        rusty_network_manager::IP4ConfigProxy::new_from_path(path.clone(), client.system_conn())
            .await
    else {
        return Vec::new();
    };
    let Ok(data) = proxy.nameserver_data().await else {
        return Vec::new();
    };
    data.iter()
        .filter_map(|ns| match &**ns.get("address")? {
            zbus::zvariant::Value::Str(s) => Some(s.to_string()),
            _ => None,
        })
        .take(2)
        .collect()
}
pub async fn wired_status(client: &NmClient) -> Option<WiredInfo> {
    use rusty_network_manager::ActiveProxy;
    for path in client.nm.active_connections().await.unwrap_or_default() {
        let Ok(proxy) = ActiveProxy::new_from_path(path.clone(), client.system_conn()).await else {
            continue;
        };
        if proxy.type_().await.as_deref() != Ok("802-3-ethernet") {
            continue;
        }
        if proxy.state().await.unwrap_or(0) != 2 {
            continue;
        }
        let id = proxy.id().await.unwrap_or_else(|_| "Wired".into());
        let device_path = proxy
            .devices()
            .await
            .ok()
            .and_then(|d| d.into_iter().next());
        let mut iface = String::new();
        if let Some(dp) = device_path.clone()
            && let Ok(dev) =
                rusty_network_manager::DeviceProxy::new_from_path(dp, client.system_conn()).await
            && let Ok(name) = dev.interface().await
        {
            iface = name;
        }
        let ip_path = proxy.ip4_config().await.ok().filter(|p| p.as_str() != "/");
        let ipv4 = match ip_path {
            Some(p) => read_first_address(client, &p).await,
            None => None,
        };
        let speed_mbps = device_speed(client, device_path).await;
        return Some(WiredInfo {
            id,
            iface,
            ipv4,
            speed_mbps,
        });
    }
    None
}
async fn device_speed(client: &NmClient, path: Option<OwnedObjectPath>) -> Option<u32> {
    use zbus::fdo::PropertiesProxy;
    use zbus::names::InterfaceName;
    let path = path?;
    let props = PropertiesProxy::builder(client.system_conn())
        .destination("org.freedesktop.NetworkManager")
        .ok()?
        .path(path)
        .ok()?
        .build()
        .await
        .ok()?;
    let all = props
        .get_all(InterfaceName::try_from("org.freedesktop.NetworkManager.Device").ok()?)
        .await
        .ok()?;
    match &**all.get("Speed")? {
        zbus::zvariant::Value::U32(v) => (*v > 0).then_some(*v),
        zbus::zvariant::Value::I32(v) => (*v > 0).then_some(*v as u32),
        _ => None,
    }
}

pub async fn hotspot_status(client: &NmClient) -> Result<Option<HotspotInfo>> {
    if let Some((_profile, map)) = super::active_hotspot_profile(client.system_conn()).await? {
        let from_map = map
            .get("802-11-wireless")
            .and_then(|w| w.get("ssid"))
            .and_then(super::bytes_to_ssid);
        let ssid = from_map.unwrap_or_else(|| "hotspot".into());
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
