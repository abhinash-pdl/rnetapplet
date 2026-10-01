use super::NmClient;
use crate::state::{Model, ModelTx};
use futures::StreamExt as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::watch;
use tracing::{info, warn};
const MIN_REFRESH_GAP: Duration = Duration::from_millis(1500);
const MIN_REFRESH_GAP_VISIBLE: Duration = Duration::from_millis(50);
const DEBOUNCE: Duration = Duration::from_millis(200);
const DEBOUNCE_FAST: Duration = Duration::from_millis(60);
const SETTLE: Duration = Duration::from_millis(150);
const PERIODIC: Duration = Duration::from_secs(45);
const PERIODIC_VISIBLE: Duration = Duration::from_secs(15);

const SCAN_MIN_INTERVAL: Duration = Duration::from_secs(10);
const CRAWL_BUDGET: Duration = Duration::from_secs(20);
const CRAWL_RETRIES: u32 = 3;
const CRAWL_BACKOFF: Duration = Duration::from_millis(600);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Refresh,
    Membership,
    Scan,
}
impl Cmd {
    fn merge(self, other: Cmd) -> Cmd {
        use Cmd::*;
        match (self, other) {
            (Scan, _) | (_, Scan) => Scan,
            (Membership, _) | (_, Membership) => Membership,
            _ => Refresh,
        }
    }
    fn is_satisfied_by(self, pending: Cmd) -> bool {
        pending == Cmd::Scan
            || pending == self
            || (self == Cmd::Refresh && pending == Cmd::Membership)
    }
}
pub struct Refresher {
    tx: watch::Sender<Cmd>,
    task: tokio::task::JoinHandle<()>,
}
impl Refresher {
    pub fn spawn(
        client: Arc<NmClient>,
        out: ModelTx,
        visible: Arc<std::sync::atomic::AtomicBool>,
        frozen: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        let (tx, rx) = watch::channel(Cmd::Refresh);
        let task = tokio::spawn(refresh_loop(
            client.clone(),
            out,
            rx,
            visible,
            frozen,
            tx.clone(),
        ));
        Self { tx, task }
    }
    pub fn request(&self, cmd: Cmd) {
        let merged = self.tx.borrow().merge(cmd);
        self.tx.send_replace(merged);
    }
    pub async fn request_and_wait(&self, cmd: Cmd, timeout: Duration) -> bool {
        self.request(cmd);
        tokio::time::timeout(timeout, async {
            loop {
                if cmd.is_satisfied_by(*self.tx.borrow()) {
                    return true;
                }
                tokio::time::sleep(MIN_REFRESH_GAP).await;
            }
        })
        .await
        .unwrap_or(false)
    }
    pub fn stop(&self) {
        self.task.abort();
    }
}
async fn refresh_loop(
    client: Arc<NmClient>,
    out: ModelTx,
    mut rx: watch::Receiver<Cmd>,
    visible: Arc<std::sync::atomic::AtomicBool>,
    frozen: Arc<std::sync::atomic::AtomicBool>,
    cmd_tx: watch::Sender<Cmd>,
) {
    let _budget = tokio::time::timeout(CRAWL_BUDGET, crawl_then_confirm(&client, &out)).await;
    let mut last = tokio::time::Instant::now();
    let mut last_scan_req = tokio::time::Instant::now()
        .checked_sub(SCAN_MIN_INTERVAL * 2)
        .unwrap_or_else(tokio::time::Instant::now);
    let scan_running = Arc::new(AtomicBool::new(false));
    loop {
        let is_visible = visible.load(Ordering::Relaxed);
        let period = if is_visible {
            PERIODIC_VISIBLE
        } else {
            PERIODIC
        };
        tokio::select! {
            _ = tokio::time::sleep(period) => {
                if frozen.load(Ordering::Relaxed) {
                    tracing::debug!("periodic tick frozen");
                    continue;
                }

                paint(&client, &out, "periodic model refresh").await;
                if is_visible {
                    kick_scan(&client, &cmd_tx, &scan_running, &mut last_scan_req);
                }
                last = tokio::time::Instant::now();
            }
            changed = rx.changed() => {
                if changed.is_err() {
                    break;
                }
                let is_visible = visible.load(Ordering::Relaxed);
                let mut pending = *rx.borrow_and_update();

                let debounce = if pending == Cmd::Membership && is_visible {
                    DEBOUNCE_FAST
                } else {
                    DEBOUNCE
                };
                while let Ok(Ok(())) =
                    tokio::time::timeout(debounce, rx.changed()).await
                {
                    pending = (*rx.borrow_and_update()).merge(pending);
                }
                let scan = pending == Cmd::Scan;
                let fast = pending == Cmd::Membership && is_visible;
                tracing::debug!(scan, "refresh firing");
                if frozen.load(Ordering::Relaxed) {
                    continue;
                }
                if !fast {
                    let gap = if is_visible {
                        MIN_REFRESH_GAP_VISIBLE
                    } else {
                        MIN_REFRESH_GAP
                    }
                    .saturating_sub(last.elapsed());
                    if !gap.is_zero() {
                        tokio::time::sleep(gap).await;
                    }
                    if frozen.load(Ordering::Relaxed) {
                        continue;
                    }
                }
                paint(&client, &out, "model refresh").await;
                if scan && !frozen.load(Ordering::Relaxed) {
                    kick_scan(&client, &cmd_tx, &scan_running, &mut last_scan_req);
                }
                last = tokio::time::Instant::now();
            }
        }
    }
}

fn kick_scan(
    client: &Arc<NmClient>,
    tx: &watch::Sender<Cmd>,
    running: &Arc<AtomicBool>,
    last_req: &mut tokio::time::Instant,
) {
    if last_req.elapsed() < SCAN_MIN_INTERVAL {
        tracing::debug!("scan skipped (NM 10s rate limit)");
        return;
    }
    if running.swap(true, Ordering::SeqCst) {
        tracing::debug!("scan skipped (one already in flight)");
        return;
    }
    *last_req = tokio::time::Instant::now();
    let client = client.clone();
    let tx = tx.clone();
    let running = running.clone();
    tokio::spawn(async move {
        scan_once(&client).await;
        running.store(false, Ordering::SeqCst);
        tx.send_modify(|cmd| *cmd = cmd.merge(Cmd::Membership));
    });
}

async fn paint(client: &NmClient, out: &ModelTx, what: &str) {
    if tokio::time::timeout(CRAWL_BUDGET, crawl_then_confirm(client, out))
        .await
        .is_err()
    {
        warn!("{what} exceeded its time budget");
    }
}

fn keep_over(current: &Model, next: &Model) -> bool {
    if !next.aps.is_empty() || current.aps.is_empty() {
        return true;
    }
    next.wifi_enabled != current.wifi_enabled
        || next.networking_enabled != current.networking_enabled
        || next.nm_online != current.nm_online
}

async fn crawl(client: &NmClient, out: &ModelTx) {
    let t0 = std::time::Instant::now();
    match client.refresh_model().await {
        Ok(model) => {
            if !keep_over(&out.borrow(), &model) {
                tracing::debug!("dropping empty crawl over populated list");
                return;
            }
            let aps = model.aps.len();
            let took = t0.elapsed();
            if tx_outdated(out, &model) {
                info!(aps, "NM model refreshed");
            }
            tracing::debug!(aps, ?took, "crawl complete");
            let _ = out.send(Arc::new(model));
        }
        Err(e) => warn!("NM refresh failed: {e:#}"),
    }
}

async fn crawl_then_confirm(client: &NmClient, out: &ModelTx) {
    crawl_settled(client, out).await;
    confirm_membership(client, out).await;
}

async fn crawl_settled(client: &NmClient, out: &ModelTx) {
    crawl(client, out).await;
    let mut previous = out.borrow().aps.len();
    for attempt in 1..=CRAWL_RETRIES {
        if previous > 0 {
            return;
        }
        tokio::time::sleep(CRAWL_BACKOFF * attempt).await;
        let Ok(model) = client.refresh_model().await else {
            return;
        };
        let aps = model.aps.len();
        if aps > 0 {
            info!(aps, attempt, "AP cache filled after retry");
            let _ = out.send(Arc::new(model));
            return;
        }
        previous = aps;
    }
    if previous == 0 {
        warn!("AP cache still empty after retries");
    }
}

fn ssid_set(aps: &[crate::state::Ap]) -> std::collections::HashSet<String> {
    aps.iter().map(|a| a.ssid.clone()).collect()
}

async fn confirm_membership(client: &NmClient, out: &ModelTx) {
    let Ok(Some(path)) = client.wifi_device_path().await else {
        return;
    };
    tokio::time::sleep(SETTLE).await;
    let Ok(aps) = client.list_aps(&path).await else {
        return;
    };
    if aps.is_empty() || ssid_set(&aps) == ssid_set(&out.borrow().aps) {
        return;
    }
    match client.refresh_model().await {
        Ok(model) => {
            if !keep_over(&out.borrow(), &model) {
                tracing::debug!("dropping empty settle correction over populated list");
                return;
            }
            info!(aps = model.aps.len(), "AP set corrected after settle");
            let _ = out.send(Arc::new(model));
        }
        Err(e) => warn!("settle correction failed: {e:#}"),
    }
}
fn tx_outdated(out: &ModelTx, model: &Model) -> bool {
    out.borrow().aps.len() != model.aps.len()
}
async fn scan_once(client: &NmClient) {
    tracing::debug!("scan requested");
    match client.wifi_device_path().await {
        Ok(Some(path)) => match client.scan_and_wait(&path, super::SCAN_WAIT).await {
            Ok(true) => tracing::debug!("scan complete"),
            Ok(false) => warn!("scan did not finish within its window"),
            Err(e) => warn!("scan request failed: {e:#}"),
        },
        Ok(None) => {}
        Err(e) => warn!("scan skipped: {e:#}"),
    }
}
pub fn spawn_signal_watcher(
    client: Arc<NmClient>,
    refresher: Arc<Refresher>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use zbus::{MatchRule, MessageStream, message::Type};
        let rules: Vec<(&str, &str, Option<&str>, Option<&str>)> = vec![
            (
                "org.freedesktop.DBus.Properties",
                "PropertiesChanged",
                None,
                Some("/org/freedesktop/NetworkManager"),
            ),
            (
                "org.freedesktop.NetworkManager.Device.Wireless",
                "AccessPointAdded",
                None,
                Some("/org/freedesktop/NetworkManager/Devices"),
            ),
            (
                "org.freedesktop.NetworkManager.Device.Wireless",
                "AccessPointRemoved",
                None,
                Some("/org/freedesktop/NetworkManager/Devices"),
            ),
        ];
        for (iface, member, arg0, ns) in rules {
            let cmd = if member.starts_with("AccessPoint") {
                Cmd::Membership
            } else {
                Cmd::Refresh
            };
            let conn = client.system_conn().clone();
            let refresher = refresher.clone();
            tokio::spawn(async move {
                let mut backoff = Duration::from_millis(500);
                loop {
                    if refresher_tx_closed(&refresher) {
                        break;
                    }
                    let rule = (|| -> anyhow::Result<MatchRule<'static>> {
                        let b = MatchRule::builder().msg_type(Type::Signal);
                        let b = b.interface(iface)?;
                        let b = b.member(member)?;
                        let b = if let Some(a) = arg0 { b.add_arg(a)? } else { b };
                        let b = if let Some(ns) = ns {
                            b.path_namespace(ns)?
                        } else {
                            b
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
                    let mut stream = match MessageStream::for_match_rule(rule, &conn, Some(64))
                        .await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(
                                "subscribe failed ({iface}.{member}), retry in {backoff:?}: {e:#}"
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(Duration::from_secs(30));
                            continue;
                        }
                    };
                    backoff = Duration::from_millis(500);
                    let mut n = 0u32;
                    while stream.next().await.is_some() {
                        n += 1;
                        tracing::debug!(n, signal = member, "ap signal");
                        refresher.request(cmd);
                    }
                    warn!("signal stream ended ({iface}.{member}), resubscribing");
                }
            });
        }
    })
}
fn refresher_tx_closed(r: &Arc<Refresher>) -> bool {
    r.tx.is_closed()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn refresh_never_downgrades_a_pending_scan() {
        let mut pending = Cmd::Refresh;
        for c in [Cmd::Refresh, Cmd::Refresh, Cmd::Scan, Cmd::Refresh] {
            pending = pending.merge(c);
        }
        assert_eq!(pending, Cmd::Scan);
    }

    #[test]
    fn a_scan_upgrades_a_pending_refresh() {
        assert_eq!(Cmd::Refresh.merge(Cmd::Scan), Cmd::Scan);
    }

    #[test]
    fn a_scan_keeps_a_pending_scan() {
        assert_eq!(Cmd::Scan.merge(Cmd::Scan), Cmd::Scan);
        assert_eq!(Cmd::Scan.merge(Cmd::Refresh), Cmd::Scan);
    }

    #[test]
    fn refreshes_stay_refreshes() {
        assert_eq!(Cmd::Refresh.merge(Cmd::Refresh), Cmd::Refresh);
    }

    #[test]
    fn a_pending_scan_satisfies_a_refresh_request() {
        assert!(Cmd::Refresh.is_satisfied_by(Cmd::Scan));
        assert!(Cmd::Scan.is_satisfied_by(Cmd::Scan));
        assert!(!Cmd::Scan.is_satisfied_by(Cmd::Refresh));
    }

    #[test]
    fn membership_beats_plain_refresh_but_loses_to_scan() {
        assert_eq!(Cmd::Refresh.merge(Cmd::Membership), Cmd::Membership);
        assert_eq!(Cmd::Membership.merge(Cmd::Refresh), Cmd::Membership);
        assert_eq!(Cmd::Membership.merge(Cmd::Scan), Cmd::Scan);
        assert_eq!(Cmd::Scan.merge(Cmd::Membership), Cmd::Scan);
    }

    #[test]
    fn a_pending_membership_satisfies_a_refresh_request() {
        assert!(Cmd::Refresh.is_satisfied_by(Cmd::Membership));
        assert!(Cmd::Membership.is_satisfied_by(Cmd::Membership));
        assert!(!Cmd::Membership.is_satisfied_by(Cmd::Refresh));
        assert!(!Cmd::Scan.is_satisfied_by(Cmd::Membership));
    }

    #[test]
    fn gap_and_debounce_are_ordered() {
        assert!(MIN_REFRESH_GAP >= DEBOUNCE);
        assert!(MIN_REFRESH_GAP < PERIODIC);
        assert!(CRAWL_BUDGET < PERIODIC);
    }

    fn ap_model(ssids: &[&str]) -> Model {
        Model {
            aps: ssids
                .iter()
                .map(|s| crate::state::Ap {
                    ssid: s.to_string(),
                    strength: 70,
                    secured: true,
                    saved: false,
                    priority: 0,
                })
                .collect::<Vec<_>>()
                .into(),
            ..Model::empty()
        }
    }

    #[test]
    fn empty_crawl_never_clobbers_a_populated_list() {
        let current = ap_model(&["Home", "Cafe"]);
        let empty = ap_model(&[]);
        assert!(!keep_over(&current, &empty));
    }

    #[test]
    fn empty_crawl_is_kept_when_the_radio_changed() {
        let current = ap_model(&["Home"]);
        let mut off = ap_model(&[]);
        off.wifi_enabled = false;
        assert!(keep_over(&current, &off));
        let mut offline = ap_model(&[]);
        offline.nm_online = false;
        assert!(keep_over(&current, &offline));
    }

    #[test]
    fn empty_over_empty_and_non_empty_over_anything_are_kept() {
        assert!(keep_over(&ap_model(&[]), &ap_model(&[])));
        assert!(keep_over(&ap_model(&["A"]), &ap_model(&["A", "B"])));
        assert!(keep_over(&ap_model(&[]), &ap_model(&["A"])));
    }
}
