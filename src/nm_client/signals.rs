use super::NmClient;
use crate::state::{Model, ModelTx};
use futures_core::Stream as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{info, warn};
const MIN_REFRESH_GAP: Duration = Duration::from_millis(1500);
const MIN_REFRESH_GAP_VISIBLE: Duration = Duration::from_millis(1000);
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
const REFRESH_BIT: u8 = 1;
const MEMBERSHIP_BIT: u8 = 2;
const SCAN_BIT: u8 = 4;
struct Pending {
    bits: AtomicU8,
    wake: Notify,
}
impl Pending {
    fn new() -> Self {
        Self {
            bits: AtomicU8::new(0),
            wake: Notify::new(),
        }
    }
    fn request(&self, cmd: Cmd) {
        let bit = match cmd {
            Cmd::Refresh => REFRESH_BIT,
            Cmd::Membership => MEMBERSHIP_BIT,
            Cmd::Scan => SCAN_BIT,
        };
        self.bits.fetch_or(bit, Ordering::Relaxed);
        self.wake.notify_one();
    }
    fn has(&self, bit: u8) -> bool {
        self.bits.load(Ordering::Relaxed) & bit != 0
    }
    fn take(&self) -> Option<Cmd> {
        match self.bits.swap(0, Ordering::Relaxed) {
            0 => None,
            b if b & SCAN_BIT != 0 => Some(Cmd::Scan),
            b if b & MEMBERSHIP_BIT != 0 => Some(Cmd::Membership),
            _ => Some(Cmd::Refresh),
        }
    }
}
pub struct Refresher {
    pending: Arc<Pending>,
    out: ModelTx,
    task: tokio::task::JoinHandle<()>,
}
impl Refresher {
    pub fn spawn(
        client: Arc<NmClient>,
        out: ModelTx,
        visible: Arc<std::sync::atomic::AtomicBool>,
        frozen: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        let pending = Arc::new(Pending::new());
        let task = tokio::spawn(refresh_loop(
            client.clone(),
            out.clone(),
            visible,
            frozen,
            pending.clone(),
        ));
        Self { pending, out, task }
    }
    pub fn request(&self, cmd: Cmd) {
        self.pending.request(cmd);
    }
    pub fn publish_with(&self, f: impl FnOnce(&mut Model)) -> bool {
        let mut m = self.out.borrow().as_ref().clone();
        f(&mut m);
        self.out.send_replace(Arc::new(m));
        true
    }
    pub fn stop(&self) {
        self.task.abort();
    }
}
async fn refresh_loop(
    client: Arc<NmClient>,
    out: ModelTx,
    visible: Arc<std::sync::atomic::AtomicBool>,
    frozen: Arc<std::sync::atomic::AtomicBool>,
    pending: Arc<Pending>,
) {
    let _budget = tokio::time::timeout(CRAWL_BUDGET, crawl_then_confirm(&client, &out)).await;
    let mut last = tokio::time::Instant::now();
    let mut last_scan_req = tokio::time::Instant::now()
        .checked_sub(SCAN_MIN_INTERVAL * 2)
        .unwrap_or_else(tokio::time::Instant::now);
    kick_scan(&client, &pending, &mut last_scan_req).await;
    loop {
        let is_visible = visible.load(Ordering::Relaxed);
        let period = if is_visible {
            PERIODIC_VISIBLE
        } else {
            PERIODIC
        };
        tokio::select! {
            _ = tokio::time::sleep(period) => {
                let scan_frozen = frozen.load(Ordering::Relaxed);
                paint(&client, &out, "periodic model refresh").await;
                let hotspot_up = out
                    .borrow()
                    .hotspot
                    .as_ref()
                    .is_some_and(|hs| hs.active);
                if !scan_frozen && !hotspot_up {
                    kick_scan(&client, &pending, &mut last_scan_req).await;
                } else {
                    tracing::debug!("periodic scan skipped");
                }
                last = tokio::time::Instant::now();
            }
            _ = pending.wake.notified() => {
                let is_visible = visible.load(Ordering::Relaxed);
                let fast = pending.has(MEMBERSHIP_BIT) && is_visible;
                let debounce = if fast { DEBOUNCE_FAST } else { DEBOUNCE };
                let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
                while tokio::time::Instant::now() < deadline
                    && tokio::time::timeout(debounce, pending.wake.notified())
                        .await
                        .is_ok()
                {
                }
                let Some(cmd) = pending.take() else {
                    continue;
                };
                let scan = cmd == Cmd::Scan;
                let fast = cmd == Cmd::Membership && is_visible;
                tracing::debug!(scan, "refresh firing");
                let scan_frozen = frozen.load(Ordering::Relaxed);
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
                }
                paint(&client, &out, "model refresh").await;
                let hotspot_up = out
                    .borrow()
                    .hotspot
                    .as_ref()
                    .is_some_and(|hs| hs.active);
                if scan && !scan_frozen && !hotspot_up {
                    kick_scan(&client, &pending, &mut last_scan_req).await;
                }
                last = tokio::time::Instant::now();
            }
        }
    }
}

async fn kick_scan(
    client: &Arc<NmClient>,
    pending: &Arc<Pending>,
    last_req: &mut tokio::time::Instant,
) {
    if last_req.elapsed() < SCAN_MIN_INTERVAL {
        tracing::debug!("scan skipped (NM 10s rate limit)");
        return;
    }
    let done = tokio::time::timeout(
        super::SCAN_WAIT + Duration::from_secs(10),
        scan_once(client),
    )
    .await;
    if matches!(done, Ok(())) {
        *last_req = tokio::time::Instant::now();
    }
    if done.is_err() {
        warn!("scan exceeded its window");
        return;
    }
    pending.request(Cmd::Membership);
}

async fn paint(client: &NmClient, out: &ModelTx, what: &str) {
    if tokio::time::timeout(CRAWL_BUDGET, crawl(client, out))
        .await
        .is_err()
    {
        warn!("{what} exceeded its time budget");
    }
}

fn keep_over(current: &Model, next: &Model, settling: bool) -> bool {
    if settling && !next.aps.is_empty() && next.aps.len() < current.aps.len() {
        return false;
    }
    if !next.aps.is_empty() || current.aps.is_empty() {
        return true;
    }
    next.wifi_enabled != current.wifi_enabled
        || next.networking_enabled != current.networking_enabled
        || next.nm_online != current.nm_online
}

fn meta_changed(current: &Model, next: &Model) -> bool {
    let mut probe = next.clone();
    probe.aps = current.aps.clone();
    *current != probe
}

fn settle_merge(current: &Model, next: Model) -> Model {
    use std::collections::HashMap;
    let mut union: HashMap<crate::state::Ssid, crate::state::Ap> = HashMap::new();
    for ap in current.aps.iter() {
        union.insert(ap.ssid.clone(), ap.clone());
    }
    for ap in next.aps.iter() {
        union.insert(ap.ssid.clone(), ap.clone());
    }
    let mut aps: Vec<crate::state::Ap> = union.into_values().collect();
    crate::state::sort_aps(&mut aps);
    Model {
        aps: aps.into(),
        ..next
    }
}

async fn crawl(client: &NmClient, out: &ModelTx) {
    let t0 = std::time::Instant::now();
    match client.refresh_model().await {
        Ok(model) => {
            let settling = client.scan_inflight();
            if !keep_over(&out.borrow(), &model, settling) {
                if settling && meta_changed(&out.borrow(), &model) {
                    let merged = settle_merge(&out.borrow(), model);
                    if **out.borrow() == merged {
                        tracing::debug!("settled merge matches current; skipping publish");
                        return;
                    }
                    tracing::debug!(
                        aps = merged.aps.len(),
                        "publishing settled merge over shrunken crawl"
                    );
                    if tx_outdated(out, &merged) {
                        info!(aps = merged.aps.len(), "NM model refreshed");
                    }
                    let _ = out.send(Arc::new(merged));
                    return;
                }
                tracing::debug!("dropping unsettled crawl over populated list");
                return;
            }
            if **out.borrow() == model {
                tracing::debug!("model unchanged; skipping publish");
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
    if !out.borrow().aps.is_empty() {
        tokio::time::sleep(SETTLE).await;
    }
    crawl_settled(client, out).await;
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

fn tx_outdated(out: &ModelTx, model: &Model) -> bool {
    let cur = out.borrow();
    if cur.aps.len() != model.aps.len() {
        return true;
    }
    let cur_hs = cur.hotspot.as_ref().map(|h| h.active).unwrap_or(false);
    let next_hs = model.hotspot.as_ref().map(|h| h.active).unwrap_or(false);
    cur_hs != next_hs
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
type SignalRule = (
    &'static str,
    &'static str,
    Option<&'static str>,
    Option<&'static str>,
    Cmd,
);
pub fn spawn_signal_watcher(
    client: Arc<NmClient>,
    refresher: std::sync::Weak<Refresher>,
) -> Vec<tokio::task::JoinHandle<()>> {
    use zbus::{MatchRule, MessageStream, message::Type};
    let rules: Vec<SignalRule> = vec![
        (
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            Some("org.freedesktop.NetworkManager"),
            Some("/org/freedesktop/NetworkManager"),
            Cmd::Refresh,
        ),
        (
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            Some("org.freedesktop.NetworkManager.Device"),
            Some("/org/freedesktop/NetworkManager"),
            Cmd::Refresh,
        ),
        (
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            Some("org.freedesktop.NetworkManager.Device.Wireless"),
            Some("/org/freedesktop/NetworkManager"),
            Cmd::Refresh,
        ),
        (
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            Some("org.freedesktop.NetworkManager.Connection.Active"),
            Some("/org/freedesktop/NetworkManager"),
            Cmd::Refresh,
        ),
        (
            "org.freedesktop.NetworkManager.Device.Wireless",
            "AccessPointAdded",
            None,
            Some("/org/freedesktop/NetworkManager/Devices"),
            Cmd::Membership,
        ),
        (
            "org.freedesktop.NetworkManager.Device.Wireless",
            "AccessPointRemoved",
            None,
            Some("/org/freedesktop/NetworkManager/Devices"),
            Cmd::Membership,
        ),
    ];
    rules
        .into_iter()
        .map(|(iface, member, arg0, ns, cmd)| {
            let conn = client.system_conn().clone();
            let refresher = refresher.clone();
            tokio::spawn(async move {
                let mut backoff = Duration::from_millis(500);
                loop {
                    let Some(live) = refresher.upgrade() else {
                        break;
                    };
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
                    let stream = match MessageStream::for_match_rule(rule, &conn, Some(64)).await {
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
                    tokio::pin!(stream);
                    while std::future::poll_fn(|cx| stream.as_mut().poll_next(cx))
                        .await
                        .is_some()
                    {
                        n += 1;
                        tracing::debug!(n, signal = member, "ap signal");
                        live.request(cmd);
                    }
                    warn!("signal stream ended ({iface}.{member}), resubscribing");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            })
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn take_prefers_scan_over_everything() {
        let pending = Pending::new();
        pending.request(Cmd::Refresh);
        pending.request(Cmd::Membership);
        pending.request(Cmd::Scan);
        pending.request(Cmd::Refresh);
        assert_eq!(pending.take(), Some(Cmd::Scan));
    }

    #[test]
    fn take_prefers_membership_over_refresh() {
        let pending = Pending::new();
        pending.request(Cmd::Refresh);
        pending.request(Cmd::Membership);
        assert_eq!(pending.take(), Some(Cmd::Membership));
    }

    #[test]
    fn take_consumes_the_request() {
        let pending = Pending::new();
        assert_eq!(pending.take(), None);
        pending.request(Cmd::Scan);
        assert_eq!(pending.take(), Some(Cmd::Scan));
        assert_eq!(pending.take(), None);
    }

    #[test]
    fn take_reports_plain_refresh() {
        let pending = Pending::new();
        pending.request(Cmd::Refresh);
        assert_eq!(pending.take(), Some(Cmd::Refresh));
    }

    #[test]
    fn gap_and_debounce_are_ordered() {
        assert!(MIN_REFRESH_GAP >= DEBOUNCE);
        assert!(MIN_REFRESH_GAP < PERIODIC);
        assert!(CRAWL_BUDGET < PERIODIC);
    }

    fn ap_model(ssids: &[&str]) -> Model {
        use crate::nm_client::aps::{Bands, Security};
        Model {
            aps: ssids
                .iter()
                .map(|s| crate::state::Ap {
                    ssid: (*s).into(),
                    strength: 70,
                    security: Security::Wpa,
                    freq_mhz: None,
                    bands: Bands::NONE,
                    saved: false,
                    known: false,
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
        assert!(!keep_over(&current, &empty, false));
        assert!(!keep_over(&current, &empty, true));
    }

    #[test]
    fn empty_crawl_is_kept_when_the_radio_changed() {
        let current = ap_model(&["Home"]);
        let mut off = ap_model(&[]);
        off.wifi_enabled = false;
        assert!(keep_over(&current, &off, false));
        let mut offline = ap_model(&[]);
        offline.nm_online = false;
        assert!(keep_over(&current, &offline, true));
    }

    #[test]
    fn empty_over_empty_and_non_empty_over_anything_are_kept() {
        assert!(keep_over(&ap_model(&[]), &ap_model(&[]), false));
        assert!(keep_over(&ap_model(&["A"]), &ap_model(&["A", "B"]), false));
        assert!(keep_over(&ap_model(&[]), &ap_model(&["A"]), false));
    }

    #[test]
    fn shrunken_crawl_is_dropped_while_a_scan_settles() {
        let full = ap_model(&["A", "B", "C"]);
        let partial = ap_model(&["A"]);
        assert!(!keep_over(&full, &partial, true));
        assert!(keep_over(&full, &partial, false));
    }

    #[test]
    fn grown_or_equal_crawl_is_kept_while_a_scan_settles() {
        let full = ap_model(&["A", "B", "C"]);
        assert!(keep_over(&full, &ap_model(&["A", "B", "C", "D"]), true));
        assert!(keep_over(&full, &full.clone(), true));
        assert!(keep_over(&ap_model(&[]), &ap_model(&["A"]), true));
    }

    #[test]
    fn meta_changed_ignores_ap_rows() {
        let a = ap_model(&["A", "B"]);
        let mut b = ap_model(&["A"]);
        assert!(!meta_changed(&a, &b));
        b = ap_model(&["A", "B"]);
        assert!(!meta_changed(&a, &b));
        let mut c = ap_model(&["A", "B"]);
        c.no_internet = true;
        assert!(meta_changed(&a, &c));
    }

    #[test]
    fn settle_merge_unions_rows_and_takes_fresh_metadata() {
        let mut current = ap_model(&["A", "B", "C"]);
        let mut next = ap_model(&["A"]);
        next.no_internet = true;
        let merged = settle_merge(&current, next.clone());
        let mut got: Vec<String> = merged.aps.iter().map(|ap| ap.ssid.to_string()).collect();
        got.sort();
        assert_eq!(got, vec!["A", "B", "C"]);
        assert!(merged.no_internet);
        current.no_internet = true;
        let same = settle_merge(&current, next.clone());
        assert!(!meta_changed(&current, &same));
    }
}
