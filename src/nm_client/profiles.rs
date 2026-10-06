use super::{get_settings, map_bounded};
use anyhow::Result;
use std::collections::HashMap;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
pub type SettingsMap = HashMap<String, HashMap<String, OwnedValue>>;
pub async fn wireless_profile_paths(conn: &zbus::Connection) -> Vec<OwnedObjectPath> {
    let Ok(settings) = rusty_network_manager::SettingsProxy::new(conn).await else {
        return Vec::new();
    };
    let Ok(paths) = settings.list_connections().await else {
        return Vec::new();
    };
    load_wireless_settings(conn, paths)
        .await
        .into_iter()
        .map(|(p, _)| p)
        .collect()
}
pub async fn load_wireless_settings(
    conn: &zbus::Connection,
    paths: Vec<OwnedObjectPath>,
) -> Vec<(OwnedObjectPath, SettingsMap)> {
    map_bounded(paths, |path| {
        let conn = conn.clone();
        async move { get_settings(&conn, &path).await.map(|m| (path, m)) }
    })
    .await
    .into_iter()
    .flatten()
    .collect()
}
pub fn wireless_ssid(map: &SettingsMap) -> Option<String> {
    map.get("802-11-wireless")
        .and_then(|w| w.get("ssid"))
        .and_then(super::bytes_to_ssid)
}
pub fn is_hotspot_profile(map: &SettingsMap) -> bool {
    map.get("802-11-wireless")
        .and_then(|w| w.get("mode"))
        .map(|v| matches!(&**v, Value::Str(s) if s.as_str() == "ap"))
        .unwrap_or(false)
}
pub fn profile_psk(map: &SettingsMap) -> Option<String> {
    map.get("802-11-wireless-security")
        .and_then(|w| w.get("psk"))
        .and_then(|v| match &**v {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        })
}
pub fn autoconnect_priority(map: &SettingsMap) -> i32 {
    map.get("connection")
        .and_then(|c| c.get("autoconnect-priority"))
        .and_then(|v| match &**v {
            Value::I32(p) => Some(*p),
            Value::U32(p) => i32::try_from(*p).ok(),
            Value::I64(p) => i32::try_from(*p).ok(),
            Value::U64(p) => i32::try_from(*p).ok(),
            Value::U16(p) => Some(*p as i32),
            Value::I16(p) => Some(*p as i32),
            _ => None,
        })
        .unwrap_or(0)
}
pub async fn saved_profile_path(
    conn: &zbus::Connection,
    ssid: &str,
) -> Result<Option<OwnedObjectPath>> {
    let loaded = load_wireless_settings(conn, wireless_profile_paths(conn).await).await;
    Ok(loaded
        .into_iter()
        .find(|(_, m)| !is_hotspot_profile(m) && wireless_ssid(m).as_deref() == Some(ssid))
        .map(|(p, _)| p))
}
pub async fn hotspot_profile_path(conn: &zbus::Connection) -> Result<Option<OwnedObjectPath>> {
    let loaded = load_wireless_settings(conn, wireless_profile_paths(conn).await).await;
    Ok(loaded
        .into_iter()
        .find(|(_, m)| is_hotspot_profile(m))
        .map(|(p, _)| p))
}
pub async fn delete_all_hotspot_profiles(conn: &zbus::Connection) {
    use rusty_network_manager::SettingsConnectionProxy;
    let loaded = load_wireless_settings(conn, wireless_profile_paths(conn).await).await;
    for (path, map) in loaded {
        if !is_hotspot_profile(&map) {
            continue;
        }
        if let Ok(proxy) = SettingsConnectionProxy::new_from_path(path, conn).await {
            let _ = proxy.delete().await;
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn v<T: Into<Value<'static>>>(t: T) -> OwnedValue {
        OwnedValue::try_from(t.into()).unwrap()
    }
    fn map() -> SettingsMap {
        HashMap::from([
            (
                "802-11-wireless".to_string(),
                HashMap::from([
                    ("ssid".to_string(), v(b"Home".to_vec())),
                    ("mode".to_string(), v("infrastructure")),
                ]),
            ),
            (
                "connection".to_string(),
                HashMap::from([("timestamp".to_string(), v(1234u64))]),
            ),
        ])
    }
    #[test]
    fn reads_ssid_and_mode() {
        let m = map();
        assert_eq!(wireless_ssid(&m).as_deref(), Some("Home"));
        assert!(!is_hotspot_profile(&m));
    }
    #[test]
    fn detects_hotspot_profile() {
        let mut m = map();
        m.get_mut("802-11-wireless")
            .unwrap()
            .insert("mode".to_string(), v("ap"));
        assert!(is_hotspot_profile(&m));
    }
    #[test]
    fn reads_autoconnect_priority() {
        let mut m = map();
        assert_eq!(
            autoconnect_priority(&m),
            0,
            "absent means NetworkManager default"
        );
        m.get_mut("connection")
            .unwrap()
            .insert("autoconnect-priority".to_string(), v(90i32));
        assert_eq!(autoconnect_priority(&m), 90);
    }
    #[test]
    fn priority_accepts_wide_integers() {
        let mut m = map();
        m.get_mut("connection")
            .unwrap()
            .insert("autoconnect-priority".to_string(), v(-5i64));
        assert_eq!(autoconnect_priority(&m), -5);
    }
    #[test]
    fn missing_fields_are_safe() {
        let empty = SettingsMap::new();
        assert_eq!(wireless_ssid(&empty), None);
        assert!(!is_hotspot_profile(&empty));
        assert_eq!(profile_psk(&empty), None);
        assert_eq!(autoconnect_priority(&empty), 0);
    }
}
