use std::time::Duration;

use anyhow::Result;
use tracing::{info, warn};

use super::NmClient;
use crate::state::ModelTx;

pub fn spawn_model_loop(client: std::sync::Arc<NmClient>, tx: ModelTx) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {

        match client.refresh_model().await {
            Ok(model) => {
                info!(
                    aps = model.aps.len(),
                    active = ?model.active_ssid,
                    "initial NM model loaded"
                );
                let _ = tx.send(model);
            }
            Err(e) => warn!("initial NM refresh failed: {e:#}"),
        }

        let mut tick = tokio::time::interval(Duration::from_secs(30));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match refresh_once(&client).await {
                Ok(Some(model)) => {
                    let _ = tx.send(model);
                }
                Ok(None) => {}
                Err(e) => warn!("periodic NM refresh failed: {e:#}"),
            }
        }
    })
}

pub async fn refresh_once(
    client: &NmClient,
) -> Result<Option<crate::state::Model>> {
    let model = client.refresh_model().await?;
    Ok(Some(model))
}

pub fn spawn_signal_watcher(
    client: std::sync::Arc<NmClient>,
    tx: ModelTx,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use futures::StreamExt as _;
        use zbus::{message::Type, MatchRule, MessageStream};

        enum Scope<'a> {
            Path(&'a str),
            Namespace(&'a str),
        }
        let rules: Vec<(&str, &str, Scope)> = vec![
            (
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                Scope::Path("/org/freedesktop/NetworkManager"),
            ),
            (
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                Scope::Namespace("/org/freedesktop/NetworkManager/Devices"),
            ),
            (
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                Scope::Namespace("/org/freedesktop/NetworkManager/ActiveConnection"),
            ),
            (
                "org.freedesktop.NetworkManager.Device.Wireless",
                "AccessPointAdded",
                Scope::Namespace("/org/freedesktop/NetworkManager/Devices"),
            ),
            (
                "org.freedesktop.NetworkManager.Device.Wireless",
                "AccessPointRemoved",
                Scope::Namespace("/org/freedesktop/NetworkManager/Devices"),
            ),
            (
                "org.freedesktop.NetworkManager.Device",
                "StateChanged",
                Scope::Namespace("/org/freedesktop/NetworkManager/Devices"),
            ),
        ];

        let (trig_tx, mut trig_rx) = tokio::sync::mpsc::channel::<()>(4);
        for (iface, member, scope) in rules {
            let conn = client.system_conn().clone();
            let trig_tx = trig_tx.clone();
            tokio::spawn(async move {

                let mut backoff = Duration::from_secs(1);
                loop {
                    if trig_tx.is_closed() {
                        break;
                    }
                    let rule = (|| -> anyhow::Result<MatchRule<'static>> {
                        let b = MatchRule::builder().msg_type(Type::Signal);
                        let b = b.interface(iface)?;
                        let b = b.member(member)?;
                        let b = match scope {
                            Scope::Path(p) => b.path(p)?,
                            Scope::Namespace(ns) => b.path_namespace(ns)?,
                        };
                        Ok(b.build())
                    })();
                    let rule = match rule {
                        Ok(r) => r,
                        Err(e) => {
                            warn!("bad signal rule {iface}.{member}: {e:#}");
                            return;
                        }
                    };
                    let mut stream =
                        match MessageStream::for_match_rule(rule, &conn, Some(16)).await
                        {
                            Ok(s) => s,
                            Err(e) => {
                                warn!("signal subscribe failed ({iface}.{member}), retry in {backoff:?}: {e:#}");
                                tokio::time::sleep(backoff).await;
                                backoff = (backoff * 2).min(Duration::from_secs(30));
                                continue;
                            }
                        };
                    backoff = Duration::from_secs(1);
                    while stream.next().await.is_some() {

                        let _ = trig_tx.try_send(());
                    }
                    warn!("signal stream ended ({iface}.{member}), resubscribing");
                }
            });
        }
        drop(trig_tx);

        while trig_rx.recv().await.is_some() {

            tokio::time::sleep(Duration::from_millis(400)).await;
            while trig_rx.try_recv().is_ok() {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            tracing::debug!("signal-driven model refresh");
            match refresh_once(&client).await {
                Ok(Some(model)) => {
                    if tx.send(model).is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(e) => warn!("signal-driven refresh failed: {e:#}"),
            }
        }
    })
}
