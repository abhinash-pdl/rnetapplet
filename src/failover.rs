use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::state::{BackendCmd, Model, ModelRx};

pub(crate) const GRACE_BAD: Duration = Duration::from_secs(10);
pub(crate) const COOLDOWN: Duration = Duration::from_secs(3 * 60);
pub(crate) const AVOID_FOR: Duration = Duration::from_secs(5 * 60);
pub(crate) const MIN_CANDIDATE_STRENGTH: u8 = 25;
pub(crate) const MAX_STRIKES: u32 = 3;
pub(crate) const MIN_SINCE_CONNECT: Duration = Duration::from_secs(30);
pub(crate) const DISCONNECTED_GRACE: Duration = Duration::from_secs(10);
pub(crate) const RECONNECT_COOLDOWN: Duration = Duration::from_secs(60);
pub(crate) const RECONNECT_COOLDOWN_LONG: Duration = Duration::from_secs(5 * 60);
pub(crate) const MIN_OPEN_STRENGTH: u8 = 40;
pub(crate) const MAX_RECONNECT_FAILURES: u32 = 5;
pub(crate) const MANUAL_LEFT_WINDOW: Duration = Duration::from_secs(60);
const STRIKE_WINDOW: Duration = Duration::from_secs(45);

pub(crate) struct Failover {
    bad_since: Option<(String, Instant)>,
    last_switch: Option<Instant>,
    avoid: HashMap<String, Instant>,
    strikes: u32,
    switched_to: Option<(String, Instant)>,
}

impl Failover {
    pub(crate) fn new() -> Self {
        Self {
            bad_since: None,
            last_switch: None,
            avoid: HashMap::new(),
            strikes: 0,
            switched_to: None,
        }
    }

    pub(crate) fn evaluate(
        &mut self,
        m: &Model,
        now: Instant,
        last_connect: Option<Instant>,
    ) -> Option<String> {
        self.avoid.retain(|_, until| now < *until);
        if let Some((target, at)) = self.switched_to.clone()
            && now.duration_since(at) > STRIKE_WINDOW
        {
            let settled = m.active_ssid.as_deref() == Some(target.as_str()) && !m.no_internet;
            if !settled {
                self.strikes += 1;
                self.avoid.insert(target, now + AVOID_FOR);
                tracing::warn!(
                    strikes = self.strikes,
                    "auto-failover target did not settle"
                );
            }
            self.switched_to = None;
        }
        let cur = m.active_ssid.clone()?;
        let eligible = m.nm_online
            && m.networking_enabled
            && m.wifi_enabled
            && m.activating_ssid.is_none()
            && !m.hotspot.as_ref().is_some_and(|h| h.active)
            && !m.vpn_connections.iter().any(|v| v.active)
            && !m.primary_wired;
        if !eligible {
            self.bad_since = None;
            return None;
        }
        if !m.no_internet {
            self.bad_since = None;
            self.strikes = 0;
            return None;
        }
        match &self.bad_since {
            Some((ssid, _)) if ssid == &cur => {}
            _ => {
                tracing::debug!(ssid = cur.as_str(), "connection unhealthy, watching");
                self.bad_since = Some((cur, now));
                return None;
            }
        }
        let since = self.bad_since.as_ref().map(|(_, t)| *t).unwrap_or(now);
        if now.duration_since(since) < GRACE_BAD {
            return None;
        }
        if self
            .last_switch
            .is_some_and(|t| now.duration_since(t) < COOLDOWN)
        {
            tracing::debug!("auto-failover waiting out cooldown");
            return None;
        }
        if self.strikes >= MAX_STRIKES {
            tracing::debug!("auto-failover stood down after strikes");
            return None;
        }
        if last_connect.is_some_and(|t| now.duration_since(t) < MIN_SINCE_CONNECT) {
            tracing::debug!("auto-failover deferring to recent connect");
            return None;
        }
        let mut candidates: Vec<(&str, u8)> = m
            .aps
            .iter()
            .filter(|ap| ap.ssid != cur && ap.saved && ap.strength >= MIN_CANDIDATE_STRENGTH)
            .filter(|ap| self.avoid.get(&ap.ssid).is_none_or(|until| now >= *until))
            .map(|ap| (ap.ssid.as_str(), ap.strength))
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let Some((target, _)) = candidates.into_iter().next() else {
            tracing::debug!(ssid = cur.as_str(), "auto-failover found no better network");
            return None;
        };
        self.last_switch = Some(now);
        self.avoid.insert(cur.clone(), now + AVOID_FOR);
        self.bad_since = None;
        self.switched_to = Some((target.to_string(), now));
        tracing::info!(
            from = cur,
            to = target,
            "auto-failover switching to a working network"
        );
        Some(target.to_string())
    }
}

pub(crate) fn spawn(
    mut rx: ModelRx,
    ev_tx: async_channel::Sender<BackendCmd>,
    last_connect: Arc<tokio::sync::Mutex<Option<Instant>>>,
    last_manual_disconnect: Arc<tokio::sync::Mutex<Option<Instant>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut fo = Failover::new();
        let mut ac = AutoConnect::new();
        loop {
            if rx.changed().await.is_err() {
                break;
            }
            let m = rx.borrow().clone();
            let now = Instant::now();
            let connected = *last_connect.lock().await;
            if let Some(target) = fo.evaluate(&m, now, connected) {
                if ev_tx.send(BackendCmd::ConnectSaved(target)).await.is_err() {
                    break;
                }
                continue;
            }
            let manual = *last_manual_disconnect.lock().await;
            if let Some((target, open)) = ac.pick(&m, now, connected, manual) {
                let cmd = if open {
                    BackendCmd::ConnectOpen(target)
                } else {
                    BackendCmd::ConnectSaved(target)
                };
                if ev_tx.send(cmd).await.is_err() {
                    break;
                }
            }
        }
    })
}

pub(crate) struct AutoConnect {
    disconnected_since: Option<Instant>,
    last_attempt: Option<Instant>,
    failures: u32,
    attempted: Option<(String, Instant)>,
    avoid: HashMap<String, Instant>,
    last_seen: Option<String>,
}

impl AutoConnect {
    pub(crate) fn new() -> Self {
        Self {
            disconnected_since: None,
            last_attempt: None,
            failures: 0,
            attempted: None,
            avoid: HashMap::new(),
            last_seen: None,
        }
    }

    pub(crate) fn pick(
        &mut self,
        m: &Model,
        now: Instant,
        last_connect: Option<Instant>,
        last_manual_disconnect: Option<Instant>,
    ) -> Option<(String, bool)> {
        self.avoid.retain(|_, until| now < *until);
        if let Some((ssid, at)) = self.attempted.clone()
            && now.duration_since(at) > STRIKE_WINDOW
        {
            if m.active_ssid.as_deref() != Some(ssid.as_str()) {
                self.failures += 1;
                self.avoid.insert(ssid, now + AVOID_FOR);
            } else {
                self.failures = 0;
            }
            self.attempted = None;
        }
        if let Some(active) = m.active_ssid.clone() {
            self.last_seen = Some(active);
            self.disconnected_since = None;
        }
        let eligible = m.nm_online
            && m.networking_enabled
            && m.wifi_enabled
            && m.activating_ssid.is_none()
            && !m.hotspot.as_ref().is_some_and(|h| h.active)
            && !m.primary_wired
            && m.active_ssid.is_none();
        if !eligible {
            self.disconnected_since = None;
            return None;
        }
        if let Some(prev) = self.last_seen.take()
            && last_manual_disconnect.is_some_and(|t| now.duration_since(t) < MANUAL_LEFT_WINDOW)
        {
            self.avoid.insert(prev, now + AVOID_FOR);
        }
        match self.disconnected_since {
            Some(since) if now.duration_since(since) >= DISCONNECTED_GRACE => {}
            Some(_) => return None,
            None => {
                self.disconnected_since = Some(now);
                return None;
            }
        }
        if last_connect.is_some_and(|t| now.duration_since(t) < MIN_SINCE_CONNECT) {
            return None;
        }
        let cooldown = if self.failures >= MAX_RECONNECT_FAILURES {
            RECONNECT_COOLDOWN_LONG
        } else {
            RECONNECT_COOLDOWN
        };
        if self
            .last_attempt
            .is_some_and(|t| now.duration_since(t) < cooldown)
        {
            return None;
        }
        let mut saved: Vec<(&str, u8)> = m
            .aps
            .iter()
            .filter(|ap| ap.saved && ap.strength >= MIN_CANDIDATE_STRENGTH)
            .filter(|ap| self.avoid.get(&ap.ssid).is_none_or(|until| now >= *until))
            .map(|ap| (ap.ssid.as_str(), ap.strength))
            .collect();
        saved.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        if let Some((ssid, _)) = saved.into_iter().next() {
            self.last_attempt = Some(now);
            self.attempted = Some((ssid.to_string(), now));
            tracing::info!(ssid, "auto-connect trying saved network");
            return Some((ssid.to_string(), false));
        }
        let mut open: Vec<(&str, u8)> = m
            .aps
            .iter()
            .filter(|ap| !ap.secured && !ap.saved && ap.strength >= MIN_OPEN_STRENGTH)
            .filter(|ap| self.avoid.get(&ap.ssid).is_none_or(|until| now >= *until))
            .map(|ap| (ap.ssid.as_str(), ap.strength))
            .collect();
        open.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        let (ssid, _) = open.into_iter().next()?;
        self.last_attempt = Some(now);
        self.attempted = Some((ssid.to_string(), now));
        tracing::info!(ssid, "auto-connect trying open network");
        Some((ssid.to_string(), true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Ap;
    use std::sync::Arc as StdArc;

    fn ap(ssid: &str, strength: u8, saved: bool) -> Ap {
        Ap {
            priority: 0,
            ssid: ssid.into(),
            strength,
            secured: true,
            enterprise: false,
            wep: false,
            freq_mhz: None,
            bands: 0,
            known: saved,
            saved,
        }
    }

    fn open_ap(ssid: &str, strength: u8) -> Ap {
        Ap {
            priority: 0,
            ssid: ssid.into(),
            strength,
            secured: false,
            enterprise: false,
            wep: false,
            freq_mhz: None,
            bands: 0,
            known: false,
            saved: false,
        }
    }
    fn model(active: Option<&str>, no_internet: bool, aps: Vec<Ap>) -> Model {
        Model {
            aps: StdArc::from(aps),
            active_ssid: active.map(str::to_string),
            no_internet,
            ..Model::empty()
        }
    }

    fn bad_model() -> Model {
        model(
            Some("Bad"),
            true,
            vec![ap("Bad", 80, true), ap("Good", 70, true)],
        )
    }

    #[test]
    fn switches_after_grace_to_strongest_saved() {
        let mut fo = Failover::new();
        let m = bad_model();
        let t0 = Instant::now();
        assert_eq!(fo.evaluate(&m, t0, None), None);
        assert_eq!(
            fo.evaluate(&m, t0 + GRACE_BAD - Duration::from_secs(1), None),
            None
        );
        assert_eq!(
            fo.evaluate(&m, t0 + GRACE_BAD + Duration::from_secs(1), None),
            Some("Good".to_string())
        );
    }

    #[test]
    fn good_state_resets() {
        let mut fo = Failover::new();
        let m = bad_model();
        let t0 = Instant::now();
        assert_eq!(fo.evaluate(&m, t0, None), None);
        let good = model(Some("Bad"), false, vec![ap("Bad", 80, true)]);
        assert_eq!(fo.evaluate(&good, t0 + GRACE_BAD * 2, None), None);
        assert_eq!(fo.evaluate(&m, t0 + GRACE_BAD * 3, None), None);
    }

    #[test]
    fn cooldown_blocks_second_switch() {
        let mut fo = Failover::new();
        let m = bad_model();
        let t0 = Instant::now();
        assert_eq!(fo.evaluate(&m, t0, None), None);
        assert_eq!(
            fo.evaluate(&m, t0 + GRACE_BAD + Duration::from_secs(1), None),
            Some("Good".to_string())
        );
        let m2 = model(
            Some("Good"),
            true,
            vec![ap("Bad", 80, true), ap("Good", 70, true)],
        );
        let t1 = t0 + GRACE_BAD + Duration::from_secs(10);
        assert_eq!(fo.evaluate(&m2, t1, None), None);
        assert_eq!(
            fo.evaluate(&m2, t1 + GRACE_BAD + Duration::from_secs(1), None),
            None
        );
    }

    #[test]
    fn skips_when_blocked() {
        let mut fo = Failover::new();
        let t0 = Instant::now();
        let t1 = t0 + GRACE_BAD * 2;
        let mut m = bad_model();
        m.wifi_enabled = false;
        assert_eq!(fo.evaluate(&m, t1, None), None);
        let mut m = bad_model();
        m.networking_enabled = false;
        assert_eq!(fo.evaluate(&m, t1, None), None);
        let mut m = bad_model();
        m.primary_wired = true;
        assert_eq!(fo.evaluate(&m, t1, None), None);
        let mut m = bad_model();
        m.vpn_connections = StdArc::from(vec![crate::state::VpnConnection {
            id: "corp".into(),
            active: true,
            path: "/x".into(),
        }]);
        assert_eq!(fo.evaluate(&m, t1, None), None);
        let mut m = bad_model();
        m.hotspot = Some(crate::state::HotspotInfo {
            ssid: "Phone".into(),
            psk: None,
            active: true,
        });
        assert_eq!(fo.evaluate(&m, t1, None), None);
        let m = model(None, true, vec![ap("Good", 70, true)]);
        assert_eq!(fo.evaluate(&m, t1, None), None);
        let m = bad_model();
        assert_eq!(fo.evaluate(&m, t0, None), None);
        assert_eq!(fo.evaluate(&m, t1, None), Some("Good".to_string()));
    }

    #[test]
    fn ignores_unsaved_and_weak_candidates() {
        let mut fo = Failover::new();
        let t0 = Instant::now();
        let m = model(
            Some("Bad"),
            true,
            vec![
                ap("Bad", 80, true),
                ap("Open", 90, false),
                ap("Weak", 10, true),
            ],
        );
        assert_eq!(fo.evaluate(&m, t0, None), None);
        assert_eq!(fo.evaluate(&m, t0 + GRACE_BAD * 2, None), None);
    }

    #[test]
    fn respects_recent_user_connect() {
        let mut fo = Failover::new();
        let m = bad_model();
        let t0 = Instant::now();
        assert_eq!(fo.evaluate(&m, t0, Some(t0)), None);
        let t1 = t0 + GRACE_BAD + Duration::from_secs(1);
        assert_eq!(fo.evaluate(&m, t1, Some(t0)), None);
        assert_eq!(
            fo.evaluate(&m, t1, Some(t0 - MIN_SINCE_CONNECT)),
            Some("Good".to_string())
        );
    }

    #[test]
    fn gives_up_after_strikes() {
        let mut fo = Failover::new();
        let m = bad_model();
        let t0 = Instant::now();
        assert_eq!(fo.evaluate(&m, t0, None), None);
        fo.strikes = MAX_STRIKES;
        assert_eq!(fo.evaluate(&m, t0 + GRACE_BAD * 2, None), None);
    }

    #[test]
    fn failed_target_is_avoided() {
        let mut fo = Failover::new();
        let m = bad_model();
        let t0 = Instant::now();
        assert_eq!(fo.evaluate(&m, t0, None), None);
        assert_eq!(
            fo.evaluate(&m, t0 + GRACE_BAD + Duration::from_secs(1), None),
            Some("Good".to_string())
        );
        let still_bad = model(
            Some("Good"),
            true,
            vec![ap("Bad", 80, true), ap("Good", 70, true)],
        );
        assert_eq!(
            fo.evaluate(
                &still_bad,
                t0 + GRACE_BAD + STRIKE_WINDOW + Duration::from_secs(60),
                None
            ),
            None
        );
        assert_eq!(fo.strikes, 1);
    }

    #[test]
    fn reconnect_waits_then_picks_strongest_saved() {
        let mut ac = AutoConnect::new();
        let m = model(None, false, vec![ap("A", 60, true), ap("B", 80, true)]);
        let t0 = Instant::now();
        assert_eq!(ac.pick(&m, t0, None, None), None);
        assert_eq!(
            ac.pick(
                &m,
                t0 + DISCONNECTED_GRACE - Duration::from_secs(1),
                None,
                None
            ),
            None
        );
        assert_eq!(
            ac.pick(
                &m,
                t0 + DISCONNECTED_GRACE + Duration::from_secs(1),
                None,
                None
            ),
            Some(("B".to_string(), false))
        );
    }

    #[test]
    fn reconnect_prefers_saved_over_open() {
        let mut ac = AutoConnect::new();
        let m = model(
            None,
            false,
            vec![open_ap("Open", 95), ap("Saved", 50, true)],
        );
        let t0 = Instant::now();
        assert_eq!(ac.pick(&m, t0, None, None), None);
        assert_eq!(
            ac.pick(
                &m,
                t0 + DISCONNECTED_GRACE + Duration::from_secs(1),
                None,
                None
            ),
            Some(("Saved".to_string(), false))
        );
    }

    #[test]
    fn reconnect_uses_strong_open_when_nothing_saved() {
        let mut ac = AutoConnect::new();
        let m = model(None, false, vec![open_ap("Open", 90), open_ap("Weak", 30)]);
        let t0 = Instant::now();
        assert_eq!(ac.pick(&m, t0, None, None), None);
        assert_eq!(
            ac.pick(
                &m,
                t0 + DISCONNECTED_GRACE + Duration::from_secs(1),
                None,
                None
            ),
            Some(("Open".to_string(), true))
        );
    }

    #[test]
    fn reconnect_skips_manually_left_network() {
        let mut ac = AutoConnect::new();
        let up = model(None, false, vec![ap("B", 80, true), ap("A", 70, true)]);
        let up = Model {
            active_ssid: Some("B".to_string()),
            ..up
        };
        let t0 = Instant::now();
        assert_eq!(ac.pick(&up, t0, None, None), None);
        let down = model(None, false, vec![ap("B", 80, true), ap("A", 70, true)]);
        let t1 = t0 + DISCONNECTED_GRACE + Duration::from_secs(1);
        assert_eq!(
            ac.pick(&down, t1, None, Some(t1 - Duration::from_secs(2))),
            None
        );
        let t2 = t1 + DISCONNECTED_GRACE + Duration::from_secs(1);
        assert_eq!(
            ac.pick(&down, t2, None, Some(t2 - Duration::from_secs(5))),
            Some(("A".to_string(), false))
        );
    }

    #[test]
    fn reconnect_rotates_after_failure() {
        let mut ac = AutoConnect::new();
        let m = model(None, false, vec![ap("A", 60, true), ap("B", 80, true)]);
        let t0 = Instant::now();
        assert_eq!(ac.pick(&m, t0, None, None), None);
        let t1 = t0 + DISCONNECTED_GRACE + Duration::from_secs(1);
        assert_eq!(ac.pick(&m, t1, None, None), Some(("B".to_string(), false)));
        let t2 = t1 + RECONNECT_COOLDOWN + Duration::from_secs(1);
        assert_eq!(ac.pick(&m, t2, None, None), Some(("A".to_string(), false)));
        assert_eq!(ac.failures, 1);
    }

    #[test]
    fn reconnect_success_resets() {
        let mut ac = AutoConnect::new();
        let m = model(None, false, vec![ap("A", 70, true)]);
        let t0 = Instant::now();
        assert_eq!(ac.pick(&m, t0, None, None), None);
        let t1 = t0 + DISCONNECTED_GRACE + Duration::from_secs(1);
        assert_eq!(ac.pick(&m, t1, None, None), Some(("A".to_string(), false)));
        let up = model(Some("A"), false, vec![ap("A", 70, true)]);
        assert_eq!(
            ac.pick(&up, t1 + STRIKE_WINDOW + Duration::from_secs(1), None, None),
            None
        );
        assert_eq!(ac.failures, 0);
    }

    #[test]
    fn reconnect_skips_blocked_states() {
        let mut ac = AutoConnect::new();
        let t1 = Instant::now() + DISCONNECTED_GRACE * 2;
        let mut m = model(None, false, vec![ap("A", 70, true)]);
        m.wifi_enabled = false;
        assert_eq!(ac.pick(&m, t1, None, None), None);
        let mut m = model(None, false, vec![ap("A", 70, true)]);
        m.hotspot = Some(crate::state::HotspotInfo {
            ssid: "Phone".into(),
            psk: None,
            active: true,
        });
        assert_eq!(ac.pick(&m, t1, None, None), None);
        let mut m = model(None, false, vec![ap("A", 70, true)]);
        m.primary_wired = true;
        assert_eq!(ac.pick(&m, t1, None, None), None);
    }

    #[test]
    fn stands_down_while_nm_is_joining() {
        let mut fo = Failover::new();
        let mut m = bad_model();
        m.activating_ssid = Some("Good".into());
        let t1 = Instant::now() + GRACE_BAD * 2;
        assert_eq!(fo.evaluate(&m, t1, None), None);
        let mut ac = AutoConnect::new();
        let mut m = model(None, false, vec![ap("A", 70, true)]);
        m.activating_ssid = Some("A".into());
        assert_eq!(ac.pick(&m, t1, None, None), None);
    }
}
