use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gtk4::prelude::*;

use super::hidden::ensure_hidden_card;
use super::hotspot::{hotspot_section_row, hotspot_shown};
use super::row::{ActiveInfo, ap_row, expander, section_label};
use super::state::{UiHandles, structural_change};
use super::vpn::vpn_row;
use crate::state::{Ap, Model};

pub(crate) const AP_MISS_THRESHOLD: u8 = 3;

pub(crate) fn merge_ap_sets(
    current: &[Ap],
    fresh: &[Ap],
    active: Option<&str>,
    misses: &mut HashMap<String, u8>,
    known: &HashMap<String, u8>,
) -> Vec<Ap> {
    if current.is_empty() || fresh.is_empty() {
        misses.clear();
        return fresh.to_vec();
    }
    let fresh_set: HashSet<&str> = fresh.iter().map(|a| a.ssid.as_str()).collect();
    let mut out: Vec<Ap> = fresh.to_vec();
    for ap in current {
        if fresh_set.contains(ap.ssid.as_str()) {
            misses.remove(&ap.ssid);
            continue;
        }
        if Some(ap.ssid.as_str()) == active {
            let mut kept = ap.clone();
            if let Some(s) = known.get(&ap.ssid) {
                kept.strength = *s;
            }
            out.push(kept);
            continue;
        }
        let n = misses.get(&ap.ssid).copied().unwrap_or(0) + 1;
        if n < AP_MISS_THRESHOLD {
            misses.insert(ap.ssid.clone(), n);
            let mut kept = ap.clone();
            if let Some(s) = known.get(&ap.ssid) {
                kept.strength = *s;
            }
            out.push(kept);
        } else {
            misses.remove(&ap.ssid);
        }
    }
    out
}

pub(crate) fn filter_aps<'a>(aps: &'a [Ap], query: &str) -> Vec<&'a Ap> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return aps.iter().collect();
    }
    aps.iter()
        .filter(|ap| ap.ssid.to_lowercase().contains(&q))
        .collect()
}

pub(crate) fn animated_card(rows: Vec<gtk4::Widget>) -> gtk4::ListBoxRow {
    let holder = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    for r in &rows {
        holder.append(r);
    }
    let inner = gtk4::ListBoxRow::new();
    inner.set_activatable(false);
    inner.set_selectable(false);
    inner.set_child(Some(&holder));
    let rev = gtk4::Revealer::new();
    rev.set_transition_type(gtk4::RevealerTransitionType::SlideDown);
    rev.set_transition_duration(130);
    rev.set_child(Some(&inner));
    rev.set_reveal_child(false);
    gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(8), {
        let rev = rev.clone();
        move || rev.set_reveal_child(true)
    });
    let wrap = gtk4::ListBoxRow::new();
    wrap.set_activatable(false);
    wrap.set_selectable(false);
    wrap.set_child(Some(&rev));
    wrap
}

pub(crate) fn update_strengths(h: &UiHandles, m: &Model) {
    let setters = h.strength_setters.borrow();
    for ap in m.aps.iter() {
        if let Some(set) = setters.get(&ap.ssid) {
            set(ap.strength);
        }
    }
}

fn ingest(h: &UiHandles, m: Arc<Model>) {
    h.prune_drafts();
    for ap in m.aps.iter() {
        h.last_strengths
            .borrow_mut()
            .insert(ap.ssid.clone(), ap.strength);
    }
    let merged = {
        let cur = h.model.borrow().clone();
        let radio_changed = m.wifi_enabled != cur.wifi_enabled
            || m.networking_enabled != cur.networking_enabled
            || m.nm_online != cur.nm_online;
        if radio_changed {
            h.ap_misses.borrow_mut().clear();
            m.aps.to_vec()
        } else {
            merge_ap_sets(
                &cur.aps,
                &m.aps,
                cur.active_ssid.as_deref(),
                &mut h.ap_misses.borrow_mut(),
                &h.last_strengths.borrow(),
            )
        }
    };
    h.last_strengths
        .borrow_mut()
        .retain(|ssid, _| merged.iter().any(|a| &a.ssid == ssid));
    let mut stored = m.as_ref().clone();
    stored.aps = merged.into();
    *h.model.borrow_mut() = Arc::new(stored);
}

pub(crate) fn apply_model(h: &UiHandles, m: Arc<Model>) {
    if m.aps.is_empty() {
        let cur = h.model.borrow();
        if !cur.aps.is_empty()
            && m.wifi_enabled == cur.wifi_enabled
            && m.networking_enabled == cur.networking_enabled
            && m.nm_online == cur.nm_online
        {
            tracing::debug!("dropping empty model over populated list");
            return;
        }
    }
    if h.refresh_blocked() {
        *h.pending_model.borrow_mut() = Some(m);
        return;
    }
    let old = h.model.borrow().clone();
    ingest(h, m);
    let structural = structural_change(&old, &h.model.borrow());
    if structural {
        request_rebuild(h);
    } else {
        update_strengths(h, &h.model.borrow());
    }
}

pub(crate) fn request_rebuild(h: &UiHandles) {
    if h.refresh_blocked() || !h.visible.get() {
        h.pending_rebuild.set(true);
        return;
    }

    refresh_list(h);
}

pub(crate) fn reset_rows(h: &UiHandles) {
    h.revealers.borrow_mut().clear();
    h.chevrons.borrow_mut().clear();
    crate::ui::motion::settle(h);
    h.connect_btns.borrow_mut().clear();
    h.submits.borrow_mut().clear();
    h.flips.borrow_mut().clear();
    h.ssid_labels.borrow_mut().clear();
    h.pw_entries.borrow_mut().clear();
    h.hotspot_entries.borrow_mut().clear();
    h.strength_setters.borrow_mut().clear();
    *h.speed_labels.borrow_mut() = None;
}

pub(crate) fn teardown(h: &UiHandles) {
    while let Some(child) = h.list.first_child() {
        h.list.remove(&child);
    }
    reset_rows(h);
    *h.hidden_card.borrow_mut() = None;
    h.hidden_expanded.set(false);
    h.hidden_closing.set(false);
    h.hotspot_was.set(false);
    h.hidden_entries.borrow_mut().clear();
    h.ap_misses.borrow_mut().clear();
    h.pw_hovered.set(false);
    h.was_editing.set(false);
    h.focus_ssid.borrow_mut().take();
    h.sync_scan_lock();
}

pub(crate) fn schedule_teardown(h: &UiHandles) {
    let h = h.clone();
    gtk4::glib::timeout_add_local_once(
        std::time::Duration::from_millis(super::POPUP_MS as u64 + 50),
        move || {
            if !h.visible.get() {
                teardown(&h);
            }
        },
    );
}

pub(crate) fn restore_focus(h: &UiHandles) {
    let target = h.focus_ssid.borrow().clone();
    let Some(entry) = target
        .as_ref()
        .and_then(|s| h.pw_entries.borrow().get(s).cloned())
    else {
        return;
    };

    if entry.is_mapped() {
        entry.grab_focus();
        return;
    }
    let done = std::cell::Cell::new(false);
    entry.connect_map(move |w| {
        if done.get() || !w.is_mapped() {
            return;
        }
        done.set(true);
        w.grab_focus();
    });
}

pub(crate) struct Visible<'a> {
    pub active: Option<&'a str>,
    pub connected: Option<&'a Ap>,
    pub available: Vec<&'a Ap>,
    pub nm_online: bool,
}

pub(crate) fn visible<'a>(m: &'a Model, query: &str) -> Visible<'a> {
    let active = UiHandles::active_ssid(m);
    let matched = filter_aps(&m.aps, query);
    let connected = active.and_then(|ssid| matched.iter().copied().find(|ap| ap.ssid == ssid));
    let available = matched
        .into_iter()
        .filter(|ap| Some(ap.ssid.as_str()) != active)
        .collect();
    Visible {
        active,
        connected,
        available,
        nm_online: m.nm_online,
    }
}

pub(crate) fn refresh_list(h: &UiHandles) {
    tracing::debug!(blocked = h.refresh_blocked(), "refresh_list invoked");
    if h.refresh_blocked() {
        h.pending_rebuild.set(true);
        return;
    }

    if h.in_refresh.get() {
        h.pending_rebuild.set(true);
        return;
    }
    h.in_refresh.set(true);
    h.pending_rebuild.set(false);
    if let Some(m) = h.pending_model.borrow_mut().take() {
        ingest(h, m);
    }
    let focus_target = h.focus_ssid.borrow().clone();
    let adj = h.scroll.vadjustment();
    let saved_pos = adj.value();

    while let Some(child) = h.list.first_child() {
        h.list.remove(&child);
    }
    reset_rows(h);

    let m = h.model.borrow().clone();
    let q = h.query.borrow().clone();
    let exp = h.expanded.borrow().clone();
    let errs = h.errors.borrow();
    let v = visible(&m, &q);
    let se = expander(h);

    if h.hidden_expanded.get() || h.hidden_closing.get() {
        let (rev, row) = ensure_hidden_card(h);
        if h.hidden_closing.get() && !h.hidden_expanded.get() {
            rev.set_reveal_child(false);
        }
        h.list.append(&row);
    }

    if !v.nm_online {
        let banner = gtk4::Label::new(Some("NetworkManager unavailable"));
        banner.set_halign(gtk4::Align::Start);
        banner.add_css_class("error");
        banner.set_wrap(true);
        banner.set_margin_start(8);
        banner.set_margin_end(8);
        banner.set_margin_top(8);
        h.list.append(&banner);
    }

    if !m.vpn_connections.is_empty() {
        h.list.append(&section_label("VPN"));
        for vpn in m.vpn_connections.iter() {
            h.list.append(&vpn_row(vpn, h));
        }
    }

    if hotspot_shown(h) {
        let hs_first = !h.hotspot_was.replace(true);
        if hs_first {
            h.list.append(&animated_card(vec![
                section_label("Hotspot").upcast(),
                hotspot_section_row(h).upcast(),
            ]));
        } else {
            h.list.append(&section_label("Hotspot"));
            h.list.append(&hotspot_section_row(h));
        }
    } else {
        h.hotspot_was.set(false);
    }

    if let Some(ssid) = v.active {
        let strength = v
            .connected
            .map(|ap| ap.strength)
            .or_else(|| h.last_strengths.borrow().get(ssid).copied())
            .unwrap_or(0);
        let synthetic = Ap {
            ssid: ssid.to_string(),
            strength,
            secured: true,
            saved: true,
            priority: 0,
        };
        let ap = v.connected.unwrap_or(&synthetic);
        h.list.append(&section_label("Connected"));
        let is_exp = exp.as_deref() == Some(ssid);
        h.list.append(&ap_row(
            ap,
            true,
            ActiveInfo {
                iface: m.active_iface.as_deref(),
                ipv4: m.active_ipv4.as_deref(),
            },
            is_exp,
            None,
            h,
            se.clone(),
        ));
    }

    let n_available = v.available.len();
    if n_available > 0 || v.active.is_none() {
        h.list.append(&section_label("Available"));
    }
    if n_available == 0 {
        let l = gtk4::Label::new(Some("No networks found"));
        l.add_css_class("dim-label");
        l.set_margin_top(16);
        h.list.append(&l);
    }
    for ap in v.available {
        let is_exp = exp.as_deref() == Some(ap.ssid.as_str());
        let err = errs.get(&ap.ssid).cloned();
        h.list.append(&ap_row(
            ap,
            false,
            ActiveInfo::none(),
            is_exp,
            err,
            h,
            se.clone(),
        ));
    }

    let adj2 = adj.clone();
    gtk4::glib::idle_add_local_once(move || {
        let max = (adj2.upper() - adj2.page_size()).max(adj2.lower());
        adj2.set_value(saved_pos.clamp(adj2.lower(), max));
    });

    if let Some(ssid) = focus_target {
        let fw = h.focus_widget();
        let within = |w: &gtk4::Widget| fw.as_ref().is_some_and(|f| f == w || f.is_ancestor(w));
        let other_entry_focused = h
            .pw_entries
            .borrow()
            .iter()
            .any(|(s, e)| s != &ssid && within(&e.clone().upcast()));
        let on_button = fw
            .as_ref()
            .and_then(|w| w.downcast_ref::<gtk4::Button>())
            .is_some();
        if h.visible.get()
            && h.expanded.borrow().as_deref() == Some(ssid.as_str())
            && !h.search_is_focused()
            && !h.hidden_is_focused()
            && !h.hotspot_is_focused()
            && !other_entry_focused
            && !on_button
        {
            *h.focus_ssid.borrow_mut() = Some(ssid);
            restore_focus(h);
        }
    }
    h.in_refresh.set(false);
    let mut rows = 0u32;
    let mut child = h.list.first_child();
    while let Some(c) = child {
        rows += 1;
        child = c.next_sibling();
    }
    tracing::debug!(rows, active = ?h.model.borrow().active_ssid, "list rebuilt");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ap(ssid: &str, strength: u8) -> Ap {
        Ap {
            ssid: ssid.into(),
            strength,
            secured: true,
            saved: false,
            priority: 0,
        }
    }

    #[test]
    fn filter_empty_returns_all() {
        let aps = vec![ap("Home", 80), ap("Cafe", 40)];
        assert_eq!(filter_aps(&aps, "").len(), 2);
    }

    #[test]
    fn filter_substring_case_insensitive() {
        let aps = vec![ap("HomeNet", 80), ap("Cafe", 40)];
        let out = filter_aps(&aps, "home");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ssid, "HomeNet");
    }

    #[test]
    fn filter_trims_and_ignores_case() {
        let aps = vec![ap("Alpha", 10), ap("Beta", 20)];
        assert_eq!(filter_aps(&aps, "  BE  ").len(), 1);
    }

    #[test]
    fn filter_borrows_instead_of_cloning() {
        let aps = vec![ap("Home", 80)];
        let out = filter_aps(&aps, "home");
        assert!(std::ptr::eq(out[0], &aps[0]));
    }

    fn merge_ctx() -> (HashMap<String, u8>, HashMap<String, u8>) {
        (HashMap::new(), HashMap::new())
    }

    fn ssids(aps: &[Ap]) -> Vec<&str> {
        aps.iter().map(|a| a.ssid.as_str()).collect()
    }

    #[test]
    fn merge_updates_strengths_and_adds_new_immediately() {
        let current = vec![ap("Home", 60), ap("Cafe", 40)];
        let fresh = vec![ap("Home", 80), ap("Work", 50)];
        let (mut misses, known) = merge_ctx();
        let mut known = known;
        known.insert("Home".into(), 80);
        let out = merge_ap_sets(&current, &fresh, None, &mut misses, &known);

        assert_eq!(ssids(&out), vec!["Home", "Work", "Cafe"]);
        assert_eq!(out[0].strength, 80);
    }

    #[test]
    fn merge_forgives_fewer_absences_than_the_threshold() {
        let current = vec![ap("Home", 60), ap("Cafe", 40)];
        let fresh = vec![ap("Home", 60)];
        let (mut misses, known) = merge_ctx();
        for _ in 0..AP_MISS_THRESHOLD - 1 {
            let out = merge_ap_sets(&current, &fresh, None, &mut misses, &known);
            assert!(ssids(&out).contains(&"Cafe"));
        }
        let out = merge_ap_sets(&current, &fresh, None, &mut misses, &known);
        assert!(!ssids(&out).contains(&"Cafe"));
    }

    #[test]
    fn merge_never_forgets_the_connected_network() {
        let current = vec![ap("Home", 60), ap("Cafe", 40)];
        let fresh = vec![ap("Home", 60)];
        let (mut misses, known) = merge_ctx();
        for _ in 0..10 {
            let out = merge_ap_sets(&current, &fresh, Some("Cafe"), &mut misses, &known);
            assert!(ssids(&out).contains(&"Cafe"));
        }
    }

    #[test]
    fn merge_resets_misses_when_an_ap_returns() {
        let current = vec![ap("Home", 60), ap("Cafe", 40)];
        let (mut misses, known) = merge_ctx();
        let partial = vec![ap("Home", 60)];
        let _ = merge_ap_sets(&current, &partial, None, &mut misses, &known);
        assert!(misses.contains_key("Cafe"));
        let full = vec![ap("Home", 60), ap("Cafe", 40)];
        let out = merge_ap_sets(&current, &full, None, &mut misses, &known);
        assert!(!misses.contains_key("Cafe"));
        assert_eq!(ssids(&out), vec!["Home", "Cafe"]);
    }

    fn model_with_active(names: &[&str], active: Option<&str>) -> Model {
        Model {
            aps: Arc::from(
                names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| Ap {
                        strength: 80 - i as u8,
                        ..ap(n, 0)
                    })
                    .collect::<Vec<_>>(),
            ),
            active_ssid: active.map(String::from),
            ..Model::empty()
        }
    }

    #[test]
    fn every_ap_except_the_active_one_is_available() {
        let names = ["Home", "Cafe", "Work", "Guest"];
        let m = model_with_active(&names, Some("Home"));
        let v = visible(&m, "");
        assert_eq!(v.active, Some("Home"));
        assert_eq!(v.connected.map(|a| a.ssid.as_str()), Some("Home"));
        let got: Vec<&str> = v.available.iter().map(|a| a.ssid.as_str()).collect();
        assert_eq!(got, vec!["Cafe", "Work", "Guest"]);
    }

    #[test]
    fn a_fourteen_network_scan_shows_thirteen_available() {
        let names = [
            "netis",
            "dishhome",
            "sudha",
            "subash",
            "subhasinik",
            "pashupati",
            "supernet",
            "tplink",
            "ntfiber",
            "reshma",
            "worknet",
            "samrat",
            "rita",
            "aarush",
        ];
        let m = model_with_active(&names, Some("dishhome"));
        let v = visible(&m, "");
        assert_eq!(v.available.len(), 13);
        assert!(!v.available.iter().any(|a| a.ssid == "dishhome"));
    }

    #[test]
    fn an_active_ssid_absent_from_the_scan_still_lists_the_rest() {
        let m = model_with_active(&["Cafe", "Work"], Some("Ghost"));
        let v = visible(&m, "");
        assert_eq!(v.active, Some("Ghost"));
        assert!(v.connected.is_none());
        assert_eq!(v.available.len(), 2);
    }

    #[test]
    fn no_active_network_makes_every_ap_available() {
        let m = model_with_active(&["Cafe", "Work"], None);
        let v = visible(&m, "");
        assert!(v.active.is_none());
        assert_eq!(v.available.len(), 2);
    }

    #[test]
    fn an_active_hotspot_hides_the_connected_section() {
        let mut m = model_with_active(&["Cafe", "Work"], Some("Cafe"));
        m.hotspot = Some(crate::state::HotspotInfo {
            ssid: "Cafe".into(),
            psk: None,
            active: true,
        });
        let v = visible(&m, "");
        assert!(v.active.is_none());
        assert_eq!(v.available.len(), 2);
    }

    #[test]
    fn the_search_query_narrows_both_sections() {
        let m = model_with_active(&["HomeNet", "Cafe", "Work"], Some("Cafe"));
        let v = visible(&m, "wor");
        assert_eq!(v.connected, None);
        assert_eq!(v.available.len(), 1);
        assert_eq!(v.available[0].ssid, "Work");
    }
}
