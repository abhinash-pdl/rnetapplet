use crate::state::UiEvent;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, oneshot};
use zbus::fdo;
use zbus::zvariant::OwnedValue;
pub const AGENT_PATH: &str = "/org/freedesktop/NetworkManager/SecretAgent";
pub const AGENT_IDENTIFIER: &str = "dev.abhinash-pdl.rnetapplet";
pub const FLAG_REQUEST_NEW: u32 = 0x2;
pub const SECRET_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_PROVIDED_PATHS: usize = 16;
const MAX_PROVIDED_PER_PATH: usize = 4;
const MAX_WAITERS: usize = 16;
struct Waiter {
    path: String,

    tx: oneshot::Sender<Option<String>>,
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
    pub async fn provide(&self, path: &str, psk: String) -> bool {
        let mut g = self.inner.lock().await;
        if g.waiters.iter().any(|w| w.path == path) {
            let mut i = 0;
            while i < g.waiters.len() {
                if g.waiters[i].path == path {
                    let w = g.waiters.remove(i);
                    let _ = w.tx.send(Some(psk.clone()));
                } else {
                    i += 1;
                }
            }
            return true;
        }
        if g.provided.len() >= MAX_PROVIDED_PATHS
            && !g.provided.contains_key(path)
            && let Some(k) = g.provided.keys().next().cloned()
        {
            g.provided.remove(&k);
        }
        let slot = g.provided.entry(path.to_string()).or_default();
        if slot.len() >= MAX_PROVIDED_PER_PATH {
            slot.remove(0);
        }
        slot.push(psk);
        false
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
            if let Some(mut queued) = g.provided.remove(path)
                && let Some(psk) = queued.pop()
            {
                if !queued.is_empty() {
                    g.provided.insert(path.to_string(), queued);
                }
                return Some(psk);
            }
            let (tx, rx) = oneshot::channel();
            if g.waiters.len() >= MAX_WAITERS {
                drop(g);
                return None;
            }
            g.waiters.push(Waiter {
                path: path.to_string(),
                tx,
            });
            rx
        };
        rx.await.ok().flatten()
    }
    pub async fn forget_path(&self, path: &str) {
        let mut g = self.inner.lock().await;
        g.provided.remove(path);
        g.waiters.retain(|w| w.path != path);
    }

    pub async fn cancel_path(&self, path: &str) {
        let mut g = self.inner.lock().await;
        let mut i = 0;
        while i < g.waiters.len() {
            if g.waiters[i].path == path {
                let w = g.waiters.remove(i);
                let _ = w.tx.send(None);
            } else {
                i += 1;
            }
        }
    }
}
pub fn extract_ssid(connection: &HashMap<String, HashMap<String, OwnedValue>>) -> Option<String> {
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
const NO_SECRETS_AVAILABLE: &str =
    "org.freedesktop.NetworkManager.SecretAgent.Error.NoSecretsAvailable";

fn defer_to_other_agents(reason: &str) -> fdo::Error {
    tracing::debug!(reason, "deferring secret to other agents");
    let named = zbus::names::OwnedErrorName::try_from(NO_SECRETS_AVAILABLE)
        .ok()
        .and_then(|name| {
            zbus::Message::method_call("/", "rnetapplet")
                .ok()
                .and_then(|b| b.build(&()).ok())
                .map(|msg| {
                    fdo::Error::from(zbus::Error::MethodError(
                        name,
                        Some(reason.to_string()),
                        msg,
                    ))
                })
        });
    named.unwrap_or_else(|| fdo::Error::Failed(reason.to_string()))
}

pub fn extract_psk(connection: &HashMap<String, HashMap<String, OwnedValue>>) -> Option<String> {
    connection
        .get("802-11-wireless-security")
        .and_then(|sec| sec.get("psk"))
        .and_then(|v| match &**v {
            zbus::zvariant::Value::Str(s) => Some(s.to_string()),
            _ => None,
        })
}

fn secrets_map(psk: &str) -> fdo::Result<HashMap<String, HashMap<String, OwnedValue>>> {
    let psk = OwnedValue::try_from(zbus::zvariant::Value::new(psk))
        .map_err(|e| fdo::Error::Failed(format!("cannot encode PSK for NM: {e}")))?;
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
        let path = connection_path.to_string();
        let request_new = flags & FLAG_REQUEST_NEW != 0;
        let cached = if request_new {
            None
        } else {
            self.store.try_take(&path).await
        };
        if let Some(psk) = cached {
            let ssid = extract_ssid(&connection).unwrap_or_default();
            tracing::info!(%ssid, %setting_name, "served stored secret");
            return secrets_map(&psk);
        }
        if setting_name != "802-11-wireless-security" {
            return Err(defer_to_other_agents("not a Wi-Fi security setting"));
        }
        let Some(ssid) = extract_ssid(&connection) else {
            return Err(defer_to_other_agents("no 802-11-wireless.ssid in request"));
        };
        tracing::info!(%ssid, %setting_name, flags, "SecretAgent GetSecrets");
        if !request_new {
            return Err(defer_to_other_agents("secret not held by this agent"));
        }
        if self
            .events
            .try_send(UiEvent::SecretsNeeded {
                ssid: ssid.clone(),
                path: path.clone(),
                request_new,
            })
            .is_err()
        {
            tracing::warn!(%ssid, "UI channel busy; cancelling secret request");
            return Err(fdo::Error::Failed("UI channel busy".into()));
        }
        match tokio::time::timeout(SECRET_TIMEOUT, self.store.wait(&path)).await {
            Ok(Some(psk)) => secrets_map(&psk),
            Ok(None) => Err(fdo::Error::Failed("secret request cancelled".into())),
            Err(_) => {
                self.store.cancel_path(&path).await;
                Err(fdo::Error::Failed("timed out waiting for password".into()))
            }
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
        connection: HashMap<String, HashMap<String, OwnedValue>>,
        connection_path: zbus::zvariant::OwnedObjectPath,
    ) -> fdo::Result<()> {
        let Some(psk) = extract_psk(&connection) else {
            tracing::debug!(%connection_path, "SaveSecrets without a psk");
            return Ok(());
        };
        self.store.provide(&connection_path.to_string(), psk).await;
        tracing::debug!(%connection_path, "SaveSecrets kept for this session");
        Ok(())
    }
    async fn delete_secrets(
        &self,
        _connection: HashMap<String, HashMap<String, OwnedValue>>,
        connection_path: zbus::zvariant::OwnedObjectPath,
    ) -> fdo::Result<()> {
        let path = connection_path.to_string();
        self.store.forget_path(&path).await;
        tracing::debug!(%connection_path, "DeleteSecrets dropped cached secret");
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
    fn psk_map(psk: &str) -> HashMap<String, HashMap<String, OwnedValue>> {
        let v = OwnedValue::try_from(zbus::zvariant::Value::new(psk.to_string())).unwrap();
        HashMap::from([(
            "802-11-wireless-security".to_string(),
            HashMap::from([("psk".to_string(), v)]),
        )])
    }

    #[test]
    fn extracts_psk_string() {
        assert_eq!(extract_psk(&psk_map("hunter2")).as_deref(), Some("hunter2"));
        assert_eq!(extract_psk(&HashMap::new()), None);
    }

    #[test]
    fn deferral_carries_nm_error_name() {
        let err = defer_to_other_agents("not ours");
        let name = match &err {
            fdo::Error::ZBus(zbus::Error::MethodError(name, _, _)) => name.to_string(),
            _ => panic!("expected a named method error, got {err:?}"),
        };
        assert_eq!(name, NO_SECRETS_AVAILABLE);
    }

    #[tokio::test]
    async fn forget_path_drops_secret_and_waiters() {
        let s = SecretStore::new();
        s.provide("/c/1", "pw1".into()).await;
        s.forget_path("/c/1").await;
        assert_eq!(s.try_take("/c/1").await, None);
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
