use super::NmClient;
use crate::state::HotspotInfo;
use anyhow::Result;
use zbus::zvariant::OwnedObjectPath;
pub async fn active_details(
    client: &NmClient,
    wifi_path: Option<OwnedObjectPath>,
) -> (Option<String>, Option<String>, Option<u32>) {
    let Some(wifi) = wifi_path else {
        return (None, None, None);
    };
    let Ok(dev) =
        rusty_network_manager::DeviceProxy::new_from_path(wifi.clone(), client.system_conn()).await
    else {
        return (None, None, None);
    };
    let iface = dev.interface().await.ok();
    let ip_path = dev.ip4_config().await.ok().filter(|p| p.as_str() != "/");
    let ipv4 = match ip_path {
        Some(p) => read_first_address(client, &p).await,
        None => None,
    };
    let bitrate =
        match rusty_network_manager::WirelessProxy::new_from_path(wifi, client.system_conn()).await
        {
            Ok(w) => w.bitrate().await.ok().filter(|b| *b > 0),
            Err(_) => None,
        };
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
