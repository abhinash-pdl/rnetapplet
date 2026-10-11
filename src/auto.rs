use crate::state::{BackendCmd, Model, ModelRx, Ssid};
use std::sync::Arc;
use std::time::{Duration, Instant};

const NUDGE_REPEAT: Duration = Duration::from_secs(30);
const MANUAL_LEFT_WINDOW: Duration = Duration::from_secs(60);
const MAX_ATTEMPTS: u32 = 3;
const BACKOFF: Duration = Duration::from_secs(5 * 60);
const SUSTAINED_OUTAGE: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub(crate) struct Decider {
    last_nudge: Option<(Ssid, Instant)>,
    attempts: u32,
    blocked_until: Option<Instant>,
    unhealthy_since: Option<Instant>,
}

impl Decider {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn decide(
        &mut self,
        m: &Model,
        now: Instant,
        last_manual_disconnect: Option<Instant>,
    ) -> Option<Ssid> {
        let eligible = m.nm_online
            && m.networking_enabled
            && m.wifi_enabled
            && !m.primary_wired
            && !m.hotspot.as_ref().is_some_and(|h| h.active);
        if !eligible {
            return None;
        }
        if !m.no_internet {
            self.reset();
            return None;
        }
        let since = *self.unhealthy_since.get_or_insert(now);
        if now.duration_since(since) < SUSTAINED_OUTAGE {
            return None;
        }
        let active = m.active_ssid.as_deref()?;
        if m.activating_ssid.is_some() {
            return None;
        }
        if last_manual_disconnect.is_some_and(|t| now.duration_since(t) < MANUAL_LEFT_WINDOW) {
            return None;
        }
        let candidate = m
            .last_good
            .as_ref()
            .filter(|lg| **lg != *active)
            .filter(|lg| m.aps.iter().any(|ap| ap.ssid == **lg && ap.saved))
            .cloned()
            .or_else(|| {
                m.aps
                    .iter()
                    .filter(|ap| ap.saved && ap.ssid != *active)
                    .max_by(|a, b| {
                        a.priority
                            .cmp(&b.priority)
                            .then_with(|| a.strength.cmp(&b.strength))
                    })
                    .map(|ap| ap.ssid.clone())
            })?;
        if self.blocked_until.is_some_and(|until| now < until) {
            return None;
        }
        if self.attempts >= MAX_ATTEMPTS {
            self.blocked_until = Some(now + BACKOFF);
            self.attempts = 0;
            return None;
        }
        if let Some((target, when)) = &self.last_nudge
            && target == &candidate
            && now.duration_since(*when) < NUDGE_REPEAT
        {
            return None;
        }
        self.attempts += 1;
        self.last_nudge = Some((candidate.clone(), now));
        Some(candidate)
    }

    fn reset(&mut self) {
        self.last_nudge = None;
        self.attempts = 0;
        self.blocked_until = None;
        self.unhealthy_since = None;
    }
}

pub(crate) fn spawn(
    mut rx: ModelRx,
    ev_tx: async_channel::Sender<BackendCmd>,
    last_manual_disconnect: Arc<tokio::sync::Mutex<Option<Instant>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut decider = Decider::new();
        loop {
            if rx.changed().await.is_err() {
                break;
            }
            let m = rx.borrow().clone();
            let manual = *last_manual_disconnect.lock().await;
            let Some(ssid) = decider.decide(&m, Instant::now(), manual) else {
                continue;
            };
            tracing::info!(ssid = %ssid.as_str(), "no-internet trigger: nudging saved network");
            if ev_tx.send(BackendCmd::ConnectSaved(ssid)).await.is_err() {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nm_client::aps::{Bands, Security};
    use crate::state::Ap;

    fn ap(ssid: &str, strength: u8, saved: bool, priority: i32) -> Ap {
        Ap {
            ssid: ssid.into(),
            bands: Bands::NONE,
            strength,
            security: Security::Wpa,
            freq_mhz: None,
            saved,
            known: saved,
            priority,
        }
    }

    fn model(
        active: Option<&str>,
        no_internet: bool,
        last_good: Option<&str>,
        aps: Vec<Ap>,
    ) -> Model {
        Model {
            aps: aps.into(),
            active_ssid: active.map(Ssid::from),
            last_good: last_good.map(Ssid::from),
            no_internet,
            nm_online: true,
            networking_enabled: true,
            wifi_enabled: true,
            primary_wired: false,
            ..Model::empty()
        }
    }

    fn now() -> Instant {
        Instant::now()
    }

    fn neglected(m: &Model, d: &mut Decider, t: Instant) -> Option<Ssid> {
        let _ = d.decide(m, t, None);
        d.decide(m, t + SUSTAINED_OUTAGE + Duration::from_secs(1), None)
    }

    #[test]
    fn healthy_network_never_nudges() {
        let m = model(
            Some("Cafe"),
            false,
            Some("Home"),
            vec![ap("Home", 70, true, 0), ap("Cafe", 60, true, 0)],
        );
        let mut d = Decider::new();
        assert_eq!(d.decide(&m, now(), None), None);
        assert_eq!(d.last_nudge, None);
    }

    #[test]
    fn disconnected_network_never_nudges() {
        let m = model(None, true, Some("Home"), vec![ap("Home", 70, true, 0)]);
        let mut d = Decider::new();
        assert_eq!(d.decide(&m, now(), None), None);
    }

    #[test]
    fn transient_outage_does_not_nudge() {
        let m = model(
            Some("Cafe"),
            true,
            Some("Home"),
            vec![ap("Home", 55, true, 0), ap("Cafe", 60, true, 0)],
        );
        let mut d = Decider::new();
        assert_eq!(d.decide(&m, now(), None), None);
        assert_eq!(
            d.decide(&m, now() + SUSTAINED_OUTAGE - Duration::from_secs(1), None),
            None
        );
    }

    #[test]
    fn nudges_last_good_first() {
        let m = model(
            Some("Cafe"),
            true,
            Some("Home"),
            vec![ap("Home", 55, true, 0), ap("Cafe", 60, true, 0)],
        );
        let mut d = Decider::new();
        assert_eq!(neglected(&m, &mut d, now()).as_deref(), Some("Home"));
    }

    #[test]
    fn skips_the_active_network() {
        let m = model(
            Some("Home"),
            true,
            Some("Home"),
            vec![ap("Home", 80, true, 0), ap("Cafe", 70, true, 0)],
        );
        let mut d = Decider::new();
        assert_eq!(neglected(&m, &mut d, now()).as_deref(), Some("Cafe"));
    }

    #[test]
    fn falls_back_to_best_saved_by_priority() {
        let m = model(
            Some("Home"),
            true,
            Some("Home"),
            vec![
                ap("Home", 80, true, 0),
                ap("Old", 30, true, 0),
                ap("Fast", 50, true, 10),
            ],
        );
        let mut d = Decider::new();
        assert_eq!(neglected(&m, &mut d, now()).as_deref(), Some("Fast"));
    }

    #[test]
    fn manual_disconnect_window_is_respected() {
        let m = model(
            Some("Home"),
            true,
            Some("Home"),
            vec![ap("Home", 80, true, 0), ap("Cafe", 70, true, 10)],
        );
        let mut d = Decider::new();
        assert_eq!(d.decide(&m, now(), Some(now())), None);
        let long_ago = now()
            .checked_sub(MANUAL_LEFT_WINDOW + Duration::from_secs(1))
            .unwrap();
        let later = now() + SUSTAINED_OUTAGE + Duration::from_secs(1);
        assert_eq!(d.decide(&m, later, Some(long_ago)).as_deref(), Some("Cafe"));
    }

    #[test]
    fn activating_and_hotspot_are_not_touched() {
        let activating = {
            let mut m = model(
                Some("Home"),
                true,
                Some("Home"),
                vec![ap("Home", 80, true, 0), ap("Cafe", 70, true, 10)],
            );
            m.activating_ssid = Some("Cafe".into());
            m
        };
        let mut d = Decider::new();
        assert_eq!(
            d.decide(&activating, now(), None),
            None,
            "a transition is already in flight"
        );
        let via_hotspot = {
            let mut m = model(
                Some("Home"),
                true,
                Some("Home"),
                vec![ap("Home", 80, true, 0), ap("Cafe", 70, true, 10)],
            );
            m.hotspot = Some(crate::state::HotspotInfo {
                ssid: "Rnet".into(),
                psk: None,
                active: true,
            });
            m
        };
        assert_eq!(
            d.decide(&via_hotspot, now(), None),
            None,
            "hotspot is serving; do not nudge"
        );
    }

    #[test]
    fn repeated_nudges_to_the_same_target_are_rate_limited() {
        let m = model(
            Some("Home"),
            true,
            Some("Home"),
            vec![ap("Home", 80, true, 0), ap("Cafe", 70, true, 10)],
        );
        let mut d = Decider::new();
        let t = now();
        assert_eq!(d.decide(&m, t, None), None);
        let armed = t + SUSTAINED_OUTAGE + Duration::from_secs(1);
        assert_eq!(d.decide(&m, armed, None).as_deref(), Some("Cafe"));
        assert_eq!(d.decide(&m, armed, None), None);
        let later = armed + NUDGE_REPEAT + Duration::from_secs(1);
        assert_eq!(d.decide(&m, later, None).as_deref(), Some("Cafe"));
        assert_eq!(d.attempts, 2);
    }

    #[test]
    fn backoff_kicks_in_after_max_attempts() {
        let m = model(
            Some("Home"),
            true,
            Some("Home"),
            vec![ap("Home", 80, true, 0), ap("Cafe", 70, true, 10)],
        );
        let mut d = Decider::new();
        let t = now();
        assert_eq!(d.decide(&m, t, None), None);
        let base = t + SUSTAINED_OUTAGE + Duration::from_secs(1);
        for i in 0..MAX_ATTEMPTS {
            let secs: u64 = (i * 31).into();
            d.decide(&m, base + Duration::from_secs(secs), None)
                .expect("within attempt budget");
        }
        assert_eq!(d.decide(&m, base + Duration::from_secs(90), None), None);
        let after_backoff = base + Duration::from_secs(91) + BACKOFF;
        assert_eq!(
            d.decide(&m, after_backoff, None).as_deref(),
            Some("Cafe"),
            "one fresh nudge is allowed once the backoff elapses"
        );
    }

    #[test]
    fn healthy_state_resets_counters() {
        let healthy = model(
            Some("Cafe"),
            false,
            Some("Home"),
            vec![ap("Home", 70, true, 0), ap("Cafe", 60, true, 0)],
        );
        let mut d = Decider::new();
        d.attempts = 2;
        d.last_nudge = Some(("Home".into(), now()));
        d.blocked_until = Some(now() + Duration::from_secs(10));
        d.unhealthy_since = Some(now());
        d.decide(&healthy, now(), None);
        assert_eq!(d.attempts, 0);
        assert_eq!(d.last_nudge, None);
        assert_eq!(d.blocked_until, None);
        assert_eq!(d.unhealthy_since, None);
    }
}
