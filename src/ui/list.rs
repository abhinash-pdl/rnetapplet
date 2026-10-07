use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gtk4::prelude::*;

use super::hidden::ensure_hidden_card;
use super::hotspot::{ensure_hotspot_card, hotspot_shown};
use super::row::{ActiveInfo, ap_row, expander, section_label, wired_row};
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
    crate::state::sort_aps(&mut out);
    out
}

pub(crate) fn filter_aps<'a>(aps: &'a [Ap], query: &str) -> Vec<&'a Ap> {
    let q = query.trim();
    if q.is_empty() {
        return aps.iter().collect();
    }
    let q = q.to_lowercase();
    aps.iter()
        .filter(|ap| ap.ssid.to_lowercase().contains(&q))
        .collect()
}

fn wifi_off_row(h: &UiHandles, airplane: bool) -> gtk4::ListBoxRow {
    let holder = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    holder.set_margin_top(18);
    holder.set_margin_bottom(18);
    holder.set_margin_start(16);
    holder.set_margin_end(16);

    let msg = if airplane {
        "Wi-Fi is off while Airplane mode is on"
    } else {
        "Wi-Fi is off"
    };
    let label = gtk4::Label::new(Some(msg));
    label.add_css_class("dim-label");
    holder.append(&label);

    if !airplane {
        let turn_on = gtk4::Button::with_label("Turn On");
        turn_on.set_halign(gtk4::Align::Center);
        let tx = h.cmd_tx.clone();
        turn_on.connect_clicked(move |_| {
            let _ = tx.try_send(crate::state::BackendCmd::SetWifi(true));
        });
        holder.append(&turn_on);
    }

    let row = gtk4::ListBoxRow::new();
    row.set_activatable(false);
    row.set_selectable(false);
    row.set_child(Some(&holder));
    row
}

pub(crate) fn update_strengths(h: &UiHandles, m: &Model) {
    let setters = h.strength_setters.borrow();
    for ap in m.aps.iter() {
        if let Some(set) = setters.get(&ap.ssid) {
            set(ap.strength);
        }
    }
}

pub(crate) fn reorder_rows(h: &UiHandles, wanted: &[String]) -> bool {
    let rows = h.rows.borrow();
    if rows.is_empty() {
        return false;
    }
    let owner = |w: &gtk4::Widget| -> Option<String> {
        rows.iter()
            .find(|(_, r)| r.upcast_ref::<gtk4::Widget>() == w)
            .map(|(s, _)| s.clone())
    };
    let mut children: Vec<gtk4::Widget> = Vec::new();
    let mut child = h.list.first_child();
    while let Some(c) = child {
        child = c.next_sibling();
        children.push(c.upcast());
    }
    let slots: Vec<(usize, String)> = children
        .iter()
        .enumerate()
        .filter_map(|(i, w)| owner(w).map(|s| (i, s)))
        .collect();
    if slots.len() != wanted.len() {
        return false;
    }
    if slots
        .iter()
        .map(|(_, s)| s.as_str())
        .eq(wanted.iter().map(String::as_str))
    {
        return false;
    }
    let reordered: Vec<gtk4::ListBoxRow> =
        wanted.iter().filter_map(|s| rows.get(s)).cloned().collect();
    if reordered.len() != wanted.len() {
        return false;
    }
    let focus = rows
        .iter()
        .find(|(_, r)| r.is_focus())
        .map(|(_, r)| r.clone());
    for (i, _) in &slots {
        h.list.remove(&children[*i]);
    }
    for (n, (i, _)) in slots.iter().enumerate() {
        h.list.insert(&reordered[n], *i as i32);
    }
    if let Some(row) = focus
        && reordered.contains(&row)
    {
        row.grab_focus();
    }
    tracing::debug!(moved = reordered.len(), "rows reordered in place");
    true
}

fn wanted_order(h: &UiHandles, m: &Model) -> Vec<String> {
    let q = h.query.borrow().clone();
    let v = visible(m, &q);
    let mut out: Vec<String> = Vec::new();
    if let Some(ap) = v.connected {
        out.push(ap.ssid.clone());
    }
    out.extend(v.available.iter().map(|ap| ap.ssid.clone()));
    out
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
    let live: HashSet<&str> = merged.iter().map(|a| a.ssid.as_str()).collect();
    h.last_strengths
        .borrow_mut()
        .retain(|ssid, _| live.contains(ssid.as_str()));
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
    if !h.visible.get() {
        ingest(h, m);
        return;
    }
    let old = h.model.borrow().clone();
    ingest(h, m);
    let structural = structural_change(&old, &h.model.borrow());
    if structural {
        request_rebuild(h);
        return;
    }
    let now = h.model.borrow().clone();
    update_strengths(h, &now);
    if crate::ui::motion::now_ms() >= h.anim_until.get() && reorder_rows(h, &wanted_order(h, &now))
    {
        h.prune_drafts();
    }
}

pub(crate) fn request_rebuild(h: &UiHandles) {
    if h.refresh_blocked() || !h.visible.get() {
        h.pending_rebuild.set(true);
        return;
    }
    if h.rebuild_queued.replace(true) {
        return;
    }
    let h = h.clone();
    gtk4::glib::idle_add_local_once(move || {
        if h.refresh_blocked() || !h.visible.get() {
            h.rebuild_queued.set(false);
            h.pending_rebuild.set(true);
            return;
        }
        let wait = h.anim_until.get() - crate::ui::motion::now_ms();
        if wait > 0 {
            let h2 = h.clone();
            gtk4::glib::timeout_add_local_once(
                std::time::Duration::from_millis((wait + 30) as u64),
                move || {
                    h2.rebuild_queued.set(false);
                    if h2.refresh_blocked() || !h2.visible.get() {
                        h2.pending_rebuild.set(true);
                        return;
                    }
                    refresh_list(&h2);
                },
            );
            return;
        }
        h.rebuild_queued.set(false);
        refresh_list(&h);
    });
}

pub(crate) fn request_rebuild_coalesced(h: &UiHandles) {
    request_rebuild(h);
}

pub(crate) fn reset_rows(h: &UiHandles) {
    h.rows.borrow_mut().clear();
    h.revealers.borrow_mut().clear();
    h.chevrons.borrow_mut().clear();
    h.status.borrow_mut().clear();

    h.action_btns.borrow_mut().clear();
    h.card_actions.borrow_mut().clear();
    h.error_labels.borrow_mut().clear();
    h.ssid_labels.borrow_mut().clear();
    h.pw_entries.borrow_mut().clear();
    h.hotspot_entries.borrow_mut().clear();
    h.strength_setters.borrow_mut().clear();
    *h.speed_labels.borrow_mut() = None;
}

pub(crate) fn teardown(h: &UiHandles) {
    reset_rows(h);
    while let Some(child) = h.list.first_child() {
        h.list.remove(&child);
    }
    *h.hidden_card.borrow_mut() = None;
    h.hidden_expanded.set(false);
    h.hidden_closing.set(false);
    *h.hotspot_card.borrow_mut() = None;
    h.hotspot_expanded.set(false);
    h.hotspot_closing.set(false);
    h.hidden_entries.borrow_mut().clear();
    h.ap_misses.borrow_mut().clear();
    h.pw_hovered.set(false);
    h.was_editing.set(false);
    h.focus_ssid.borrow_mut().take();
    h.sync_scan_lock();
}

pub(crate) fn schedule_teardown(h: &UiHandles) {
    let h = h.clone();
    gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(350), move || {
        if !h.visible.get() {
            teardown(&h);
            super::placement::trim_memory();
        }
    });
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

    reset_rows(h);
    while let Some(child) = h.list.first_child() {
        h.list.remove(&child);
    }

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

    if let Some(wired) = m.wired.as_ref() {
        h.list.append(&section_label("Wired"));
        h.list.append(&wired_row(wired, h));
    }

    if hotspot_shown(h) {
        let (_, row) = ensure_hotspot_card(h);
        h.list.append(&row);
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
            enterprise: false,
            wep: false,
            freq_mhz: None,
            bands: v
                .connected
                .map(|ap| crate::nm_client::aps::band_bit(ap.freq_mhz))
                .unwrap_or(0),
            saved: true,
            known: true,
            priority: 0,
        };
        let ap = v.connected.unwrap_or(&synthetic);
        h.list.append(&section_label("Connected"));
        let is_exp = exp.as_deref() == Some(ssid);
        let dns = m.active_dns.join(", ");
        let connected_row = ap_row(
            ap,
            true,
            ActiveInfo {
                iface: m.active_iface.as_deref(),
                ipv4: m.active_ipv4.as_deref(),
                gateway: m.active_gateway.as_deref(),
                dns: (!dns.is_empty()).then_some(dns.as_str()),
                freq_mhz: m.active_freq_mhz,
            },
            is_exp,
            None,
            h,
            se.clone(),
        );
        h.rows
            .borrow_mut()
            .insert(ssid.to_string(), connected_row.clone());
        h.list.append(&connected_row);
    }

    let wifi_off = !m.wifi_enabled;
    let n_available = v.available.len();
    if wifi_off {
        h.list.append(&wifi_off_row(h, m.airplane_mode()));
    } else {
        if n_available > 0 || v.active.is_none() {
            h.list.append(&section_label("Available"));
        }
        if n_available == 0 {
            let l = gtk4::Label::new(Some("No networks found"));
            l.add_css_class("dim-label");
            l.set_margin_top(16);
            h.list.append(&l);
        }
    }
    if !wifi_off {
        for ap in &v.available {
            let is_exp = exp.as_deref() == Some(ap.ssid.as_str());
            let err = errs.get(&ap.ssid).cloned();
            let row = ap_row(ap, false, ActiveInfo::none(), is_exp, err, h, se.clone());
            h.rows.borrow_mut().insert(ap.ssid.clone(), row.clone());
            h.list.append(&row);
        }
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
    if h.visible.get() {
        if let Some(k) = exp {
            let child = h.revealers.borrow().get(&k).and_then(|r| r.child());
            let extra = match child {
                Some(c) if h.scroll.width() > 0 => c
                    .measure(gtk4::Orientation::Vertical, h.scroll.width())
                    .1
                    .max(0),
                _ => h.extra_heights.borrow().get(&k).copied().unwrap_or(0),
            };
            (h.grow)(extra);
        } else if h.hidden_expanded.get() {
            let child = h
                .hidden_card
                .borrow()
                .as_ref()
                .and_then(|(r, _)| r.child())
                .filter(|_| h.scroll.width() > 0);
            if let Some(c) = child {
                let extra = c
                    .measure(gtk4::Orientation::Vertical, h.scroll.width())
                    .1
                    .max(0);
                (h.grow)(extra);
            }
        } else if h.hotspot_expanded.get() {
            let child = h
                .hotspot_card
                .borrow()
                .as_ref()
                .and_then(|(r, _)| r.child())
                .filter(|_| h.scroll.width() > 0);
            if let Some(c) = child {
                let extra = c
                    .measure(gtk4::Orientation::Vertical, h.scroll.width())
                    .1
                    .max(0);
                (h.grow)(extra);
            }
        }
    }
    (h.fit)();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ap(ssid: &str, strength: u8) -> Ap {
        Ap {
            ssid: ssid.into(),
            strength,
            secured: true,
            enterprise: false,
            wep: false,
            freq_mhz: None,
            bands: 0,
            saved: false,
            known: false,
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
    fn merge_puts_the_stronger_network_first() {
        let current = vec![ap("Home", 60), ap("Cafe", 40)];
        let loud = vec![ap("Home", 30), ap("Cafe", 90)];
        let (mut misses, known) = merge_ctx();
        assert_eq!(
            ssids(&merge_ap_sets(&current, &loud, None, &mut misses, &known)),
            ["Cafe", "Home"]
        );
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
        let home = out.iter().find(|a| a.ssid == "Home").expect("Home kept");
        assert_eq!(home.strength, 80);
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
