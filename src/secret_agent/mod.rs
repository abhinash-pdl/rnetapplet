use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{oneshot, Mutex};
use zbus::fdo;
use zbus::zvariant::OwnedValue;

use crate::state::UiEvent;

pub const AGENT_PATH: &str = "/org/freedesktop/NetworkManager/SecretAgent";

pub const AGENT_IDENTIFIER: &str = "dev.abhinash-pdl.rnetapplet";

pub const FLAG_REQUEST_NEW: u32 = 0x2;

pub const SECRET_TIMEOUT: Duration = Duration::from_secs(120);

struct Waiter {
    path: String,
    tx: oneshot::Sender<String>,
}

#[derive(Default)]
struct Inner {

    provided: HashMap<String, Vec<String>>,
    waiters: Vec<Waiter>,
}

#[derive(Default)]
pub struct SecretStore {
    inner: Mutex<Inner>,
}

impl SecretStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub async fn provide(&self, path: &str, psk: String) {
        let mut g = self.inner.lock().await;
        if let Some(i) = g.waiters.iter().position(|w| w.path == path) {
            let w = g.waiters.remove(i);
            let _ = w.tx.send(psk);
        } else {
            g.provided.entry(path.to_string()).or_default().push(psk);
        }
    }

    pub async fn try_take(&self, path: &str) -> Option<String> {
        let mut g = self.inner.lock().await;
        let v = g.provided.get_mut(path)?;
        let psk = v.pop()?;
        if v.is_empty() {
            g.provided.remove(path);
        }
        Some(psk)
    }

    pub async fn wait(&self, path: &str) -> Option<String> {
        let rx = {
            let mut g = self.inner.lock().await;
            let (tx, rx) = oneshot::channel();
            g.waiters.push(Waiter {
                path: path.to_string(),
                tx,
            });
            rx
        };
        rx.await.ok()
    }

    pub async fn cancel_path(&self, path: &str) {
        let mut g = self.inner.lock().await;
        g.waiters.retain(|w| w.path != path);
    }
}

pub fn extract_ssid(
    connection: &HashMap<String, HashMap<String, OwnedValue>>,
) -> Option<String> {
    let ssid_val = connection
        .get("802-11-wireless")
        .and_then(|w| w.get("ssid"))?;
    let bytes: Vec<u8> = match &**ssid_val {
        zbus::zvariant::Value::Array(arr) => {
            arr.iter().filter_map(|v| u8::try_from(v).ok()).collect()
        }
        _ => return None,
    };
    crate::nm_client::aps::decode_ssid(&bytes)
}

fn secrets_map(psk: &str) -> fdo::Result<HashMap<String, HashMap<String, OwnedValue>>> {
    let psk = OwnedValue::try_from(zbus::zvariant::Value::new(psk)).map_err(|e| {
        fdo::Error::Failed(format!("cannot encode PSK for NM: {e}"))
    })?;
    let mut sec = HashMap::new();
    sec.insert(
        "802-11-wireless-security".to_string(),
        HashMap::from([("psk".to_string(), psk)]),
    );
    Ok(sec)
}

pub struct SecretAgentImpl {
    store: Arc<SecretStore>,
    events: async_channel::Sender<UiEvent>,
}

#[zbus::interface(name = "org.freedesktop.NetworkManager.SecretAgent")]
impl SecretAgentImpl {
    async fn get_secrets(
        &self,
        connection: HashMap<String, HashMap<String, OwnedValue>>,
        connection_path: zbus::zvariant::OwnedObjectPath,
        setting_name: String,
        _hints: Vec<String>,
        flags: u32,
    ) -> fdo::Result<HashMap<String, HashMap<String, OwnedValue>>> {
        let Some(ssid) = extract_ssid(&connection) else {
            return Err(fdo::Error::Failed("no 802-11-wireless.ssid in request".into()));
        };
        tracing::info!(%ssid, %setting_name, flags, "SecretAgent GetSecrets");

        if setting_name != "802-11-wireless-security" {
            return Err(fdo::Error::Failed(format!(
                "unsupported setting {setting_name}"
            )));
        }

        let path = connection_path.to_string();
        if let Some(psk) = self.store.try_take(&path).await {
            return secrets_map(&psk);
        }

        let request_new = flags & FLAG_REQUEST_NEW != 0;
        let _ = self
            .events
            .send(UiEvent::SecretsNeeded { ssid: ssid.clone(), path: path.clone(), request_new })
            .await;
        match tokio::time::timeout(
            SECRET_TIMEOUT,
            self.store.wait(&path),
        )
        .await
        {
            Ok(Some(psk)) => secrets_map(&psk),
            Ok(None) => Err(fdo::Error::Failed("secret request cancelled".into())),
            Err(_) => Err(fdo::Error::Failed("timed out waiting for password".into())),
        }
    }

    async fn cancel_get_secrets(
        &self,
        connection_path: zbus::zvariant::OwnedObjectPath,
        setting_name: String,
    ) -> fdo::Result<()> {
        tracing::debug!(%connection_path, %setting_name, "SecretAgent CancelGetSecrets");
        self.store.cancel_path(&connection_path.to_string()).await;
        Ok(())
    }

    async fn save_secrets(
        &self,
        _connection: HashMap<String, HashMap<String, OwnedValue>>,
        connection_path: zbus::zvariant::OwnedObjectPath,
    ) -> fdo::Result<()> {

        tracing::debug!(%connection_path, "SecretAgent SaveSecrets (noop)");
        Ok(())
    }

    async fn delete_secrets(
        &self,
        _connection: HashMap<String, HashMap<String, OwnedValue>>,
        connection_path: zbus::zvariant::OwnedObjectPath,
    ) -> fdo::Result<()> {
        tracing::debug!(%connection_path, "SecretAgent DeleteSecrets (noop)");
        Ok(())
    }
}

pub async fn register(
    client: &crate::nm_client::NmClient,
    store: Arc<SecretStore>,
    events: async_channel::Sender<UiEvent>,
) -> anyhow::Result<()> {
    let agent = SecretAgentImpl { store, events };
    client
        .system_conn()
        .object_server()
        .at(AGENT_PATH, agent)
        .await?;
    let mgr = rusty_network_manager::AgentManagerProxy::new(client.system_conn()).await?;
    mgr.register_with_capabilities(AGENT_IDENTIFIER, 0).await?;
    tracing::info!("SecretAgent registered with NetworkManager");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssid_map(ssid: &[u8]) -> HashMap<String, HashMap<String, OwnedValue>> {
        let arr = zbus::zvariant::Value::new(ssid.to_vec());
        HashMap::from([(
            "802-11-wireless".to_string(),
            HashMap::from([("ssid".to_string(), OwnedValue::try_from(arr).unwrap())]),
        )])
    }

    #[test]
    fn extracts_ssid_bytes() {
        assert_eq!(extract_ssid(&ssid_map(b"Home")), Some("Home".into()));
        assert_eq!(extract_ssid(&HashMap::new()), None);
    }

    #[test]
    fn secrets_map_shape() {
        let m = secrets_map("hunter2").expect("test map");
        assert!(m.contains_key("802-11-wireless-security"));
        assert!(m["802-11-wireless-security"].contains_key("psk"));
    }

    #[tokio::test]
    async fn provide_before_wait_resolves() {
        let s = SecretStore::new();
        s.provide("/c/1", "pw1".into()).await;
        assert_eq!(s.try_take("/c/1").await.as_deref(), Some("pw1"));
        assert_eq!(s.try_take("/c/1").await, None);
    }

    #[tokio::test]
    async fn wait_then_provide_resolves() {
        let s = SecretStore::new();
        let s2 = s.clone();
        let waiter = tokio::spawn(async move { s2.wait("/c/1").await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        s.provide("/c/1", "pw2".into()).await;
        assert_eq!(waiter.await.unwrap().as_deref(), Some("pw2"));
    }

    #[tokio::test]
    async fn cancel_path_drops_waiter() {
        let s = SecretStore::new();
        let s2 = s.clone();
        let waiter = tokio::spawn(async move { s2.wait("/c/9").await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        s.cancel_path("/c/9").await;
        assert_eq!(waiter.await.unwrap(), None);
    }
}
