use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::prelude::*;

use super::list::request_rebuild;
use super::state::UiHandles;
use super::theme::{net_icon_live, themed_icon};
use crate::state::{Ap, BackendCmd};

pub(crate) const SSID_CHARS: i32 = 18;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) fn detail_line(text: &str) -> gtk4::Label {
    let l = gtk4::Label::new(Some(text));
    l.set_halign(gtk4::Align::Start);
    l.add_css_class("dim-label");
    l
}

pub(crate) fn section_label(text: &str) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.set_activatable(false);
    row.set_focusable(false);
    row.add_css_class("rnet-section-row");
    let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    hbox.set_margin_start(8);
    hbox.set_margin_end(8);
    hbox.set_margin_top(10);
    let l = gtk4::Label::new(Some(text));
    l.add_css_class("rnet-section");
    hbox.append(&l);
    let sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    sep.set_hexpand(true);
    sep.set_valign(gtk4::Align::Center);
    hbox.append(&sep);
    row.set_child(Some(&hbox));
    row
}

pub(crate) fn flat_icon_button(icon: &str, pixel: i32, tooltip: &str) -> gtk4::Button {
    let img = gtk4::Image::from_icon_name(&themed_icon(&[icon]));
    img.set_pixel_size(pixel);
    img.set_valign(gtk4::Align::Center);
    img.set_halign(gtk4::Align::Center);
    let btn = gtk4::Button::new();
    btn.set_child(Some(&img));
    btn.add_css_class("flat");
    btn.add_css_class("rnet-hbtn");
    btn.set_valign(gtk4::Align::Center);
    btn.set_halign(gtk4::Align::Center);
    btn.set_tooltip_text(Some(tooltip));
    btn.set_focusable(false);
    btn.set_focus_on_click(false);
    btn
}

pub(crate) fn action_button(
    label: &str,
    connected: bool,
    cmd: BackendCmd,
    h: &UiHandles,
) -> gtk4::Button {
    let h2 = h.clone();
    let cmd2 = cmd.clone();
    action_button_with(label, connected, move || {
        if let Some(ssid) = target_ssid(&cmd2) {
            mark_connecting(&h2, &ssid, false, false);
            set_row_connecting(&h2, &ssid, true);
        }
        let _ = h2.cmd_tx.try_send(cmd2.clone());
    })
}

pub(crate) fn action_button_with(
    label: &str,
    connected: bool,
    on_click: impl Fn() + 'static,
) -> gtk4::Button {
    let btn = gtk4::Button::new();
    if connected {
        let img = gtk4::Image::from_icon_name(&themed_icon(&["system-shutdown-symbolic"]));
        img.set_pixel_size(16);
        let text = gtk4::Label::new(Some(label));
        img.add_css_class("disconnect-label");
        text.add_css_class("disconnect-label");
        let inner = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        inner.append(&img);
        inner.append(&text);
        btn.set_child(Some(&inner));
    } else {
        btn.set_label(label);
    }
    btn.add_css_class("flat");
    btn.connect_clicked(move |_| on_click());
    btn
}

fn target_ssid(cmd: &BackendCmd) -> Option<String> {
    match cmd {
        BackendCmd::ConnectOpen(s)
        | BackendCmd::ConnectSaved(s)
        | BackendCmd::ConnectSecure { ssid: s, .. } => Some(s.clone()),
        _ => None,
    }
}

pub(crate) fn animate_expansion(h: &UiHandles, target: &Option<String>) {
    let target = target.as_deref();
    for (ssid, rev) in h.revealers.borrow().iter() {
        let open = target == Some(ssid.as_str());
        rev.set_transition_duration(crate::ui::motion::REVEAL_MS);
        rev.set_reveal_child(open);
    }
    for (ssid, img) in h.chevrons.borrow().iter() {
        let name = if target == Some(ssid.as_str()) {
            "pan-up-symbolic"
        } else {
            "pan-down-symbolic"
        };
        img.set_icon_name(Some(&themed_icon(&[name])));
    }
    for (ssid, (btn, home, anchor)) in h.action_btns.borrow().iter() {
        let _ = &anchor;
        if h.model
            .borrow()
            .aps
            .iter()
            .any(|ap| &ap.ssid == ssid && !ap.secured)
        {
            continue;
        }
        let open = target == Some(ssid.as_str());
        let Some(actions) = h.card_actions.borrow().get(ssid).cloned() else {
            continue;
        };
        btn.unparent();
        if open {
            actions.prepend(btn);
        } else {
            home.insert_child_after(btn, Some(anchor));
        }
    }
    for (ssid, l) in h.ssid_labels.borrow().iter() {
        if target == Some(ssid.as_str()) {
            l.set_ellipsize(gtk4::pango::EllipsizeMode::None);
            l.set_max_width_chars(SSID_CHARS);
        } else {
            l.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            l.set_max_width_chars(SSID_CHARS);
        }
    }
    if let Some(ssid) = target {
        let entry = h.pw_entries.borrow().get(ssid).cloned();
        if let Some(entry) = entry {
            let h2 = h.clone();
            let expanded = h.expanded.clone();
            let ssid = ssid.to_string();
            gtk4::glib::timeout_add_local_once(
                std::time::Duration::from_millis(crate::ui::motion::REVEAL_MS as u64 + 180),
                move || {
                    if !h2.visible.get() || h2.any_entry_focused() {
                        return;
                    }
                    if expanded.borrow().as_deref() == Some(ssid.as_str()) && entry.is_mapped() {
                        entry.grab_focus();
                    }
                },
            );
        }
    }
}

pub(crate) fn show_error_label(h: &UiHandles, ssid: &str, message: Option<&str>) {
    if let Some(label) = h.error_labels.borrow().get(ssid).cloned() {
        match message {
            Some(msg) => {
                label.set_text(msg);
                label.set_visible(true);
            }
            None => {
                label.set_text("");
                label.set_visible(false);
            }
        }
        return;
    }
    request_rebuild(h);
}

pub(crate) fn set_expanded(h: &UiHandles, ssid: Option<&str>) {
    let target = ssid.map(|s| s.to_string());
    if target.is_some() && h.hidden_expanded.get() {
        super::hidden::collapse_hidden(h);
    }
    let previous = h.expanded.borrow().clone();
    *h.focus_ssid.borrow_mut() = target.clone();
    *h.expanded.borrow_mut() = target.clone();
    let built = match target.as_deref() {
        Some(s) => h.revealers.borrow().contains_key(s),
        None => true,
    };
    clear_stale_errors(h, target.as_deref());
    if built {
        animate_expansion(h, &target);
        if target.is_some() {
            h.sync_focus();
        }
    } else {
        request_rebuild(h);
    }

    let key = target.clone().or_else(|| previous.clone());
    let card_child = {
        let map = h.revealers.borrow();
        key.as_ref()
            .and_then(|k| map.get(k))
            .and_then(|rev| rev.child())
    };
    let extra = match (&card_child, &key) {
        (Some(cardw), _) if h.scroll.width() > 0 => cardw
            .measure(gtk4::Orientation::Vertical, h.scroll.width())
            .1
            .max(0),
        _ => 0,
    };

    let reserved = match target.as_deref() {
        Some(_) if extra > 0 => {
            if let Some(k) = key.as_ref() {
                h.extra_heights.borrow_mut().insert(k.clone(), extra);
            }
            extra
        }

        Some(_) => h
            .extra_heights
            .borrow()
            .get(key.as_deref().unwrap_or(""))
            .copied()
            .unwrap_or(0),
        None => {
            if let Some(k) = key.as_ref() {
                h.extra_heights.borrow_mut().remove(k);
            }
            0
        }
    };
    (h.grow)(reserved);
    if let Some(ssid) = target.as_deref()
        && let Some(row) = h.rows.borrow().get(ssid).cloned()
        && let Some(bounds) = row.compute_bounds(&h.list)
    {
        let adj = h.scroll.vadjustment();
        let page = adj.page_size();
        if page > 0.0 {
            let top = f64::from(bounds.y());
            let bottom = top + f64::from(bounds.height()) + reserved as f64;
            let max = (adj.upper() - page).max(0.0);
            if bottom > adj.value() + page {
                adj.set_value((bottom - page).clamp(0.0, max));
            } else if top < adj.value() {
                adj.set_value(top.clamp(0.0, max));
            }
        }
    }
    h.anim_until
        .set(gtk4::glib::monotonic_time() / 1000 + crate::ui::motion::REVEAL_MS as i64);
    (h.fit)();

    let h2 = h.clone();
    gtk4::glib::timeout_add_local_once(
        std::time::Duration::from_millis(crate::ui::motion::REVEAL_MS as u64 + 40),
        move || {
            (h2.fit)();
        },
    );
}

pub(crate) fn expander(h: &UiHandles) -> Rc<dyn Fn(Option<String>)> {
    let h = h.clone();
    Rc::new(move |v| {
        set_expanded(&h, v.as_deref());
    })
}

pub(crate) fn flash_error(h: &UiHandles, ssid: &str, message: &str) {
    h.errors
        .borrow_mut()
        .insert(ssid.to_string(), message.to_string());
    show_error_label(h, ssid, Some(message));
}

pub(crate) fn clear_stale_errors(h: &UiHandles, keep: Option<&str>) {
    let stale: Vec<String> = h
        .errors
        .borrow()
        .keys()
        .filter(|s| Some(s.as_str()) != keep)
        .cloned()
        .collect();
    for ssid in stale {
        h.errors.borrow_mut().remove(&ssid);
        show_error_label(h, &ssid, None);
    }
}

pub(crate) fn set_row_connecting(h: &UiHandles, ssid: &str, on: bool) {
    if let Some(status) = h.status.borrow().get(ssid) {
        status.set_visible(on);
        let mut child = status.first_child();
        while let Some(w) = child {
            if let Some(spin) = w.downcast_ref::<gtk4::Spinner>() {
                if on {
                    spin.start();
                } else {
                    spin.stop();
                }
                break;
            }
            child = w.next_sibling();
        }
    }
    if let Some((btn, _, _)) = h.action_btns.borrow().get(ssid) {
        btn.set_sensitive(!on);
    }
}

pub(crate) fn mark_connecting(h: &UiHandles, ssid: &str, with_password: bool, unsaved: bool) {
    h.connecting
        .borrow_mut()
        .insert(ssid.to_string(), Instant::now() + CONNECT_TIMEOUT);
    if with_password {
        h.pw_attempt.borrow_mut().insert(ssid.to_string());
    }
    if unsaved {
        h.unsaved_attempt.borrow_mut().insert(ssid.to_string());
    }
    h.errors.borrow_mut().remove(ssid);

    {
        let h2 = h.clone();
        let ssid = ssid.to_string();
        gtk4::glib::timeout_add_local(Duration::from_secs(1), move || {
            if h2.connecting.borrow().contains_key(&ssid) {
                let _ = h2.cmd_tx.try_send(BackendCmd::Refresh);
                gtk4::glib::ControlFlow::Continue
            } else {
                gtk4::glib::ControlFlow::Break
            }
        });
    }
    let h2 = h.clone();
    let ssid = ssid.to_string();
    gtk4::glib::timeout_add_local_once(CONNECT_TIMEOUT, move || {
        let expired = h2
            .connecting
            .borrow()
            .get(&ssid)
            .is_some_and(|d| Instant::now() >= *d);
        let still_off = h2.model.borrow().active_ssid.as_deref() != Some(ssid.as_str());
        if !expired || !still_off {
            return;
        }
        h2.connecting.borrow_mut().remove(&ssid);
        let used_pw = h2.pw_attempt.borrow_mut().remove(&ssid);
        let was_unsaved = h2.unsaved_attempt.borrow_mut().remove(&ssid);
        *h2.expanded.borrow_mut() = Some(ssid.clone());
        *h2.focus_ssid.borrow_mut() = Some(ssid.clone());
        if was_unsaved {
            let _ = h2.cmd_tx.try_send(BackendCmd::Forget(ssid.clone()));
        }
        if used_pw {
            flash_error(&h2, &ssid, "Incorrect password");
        } else {
            h2.errors
                .borrow_mut()
                .insert(ssid.clone(), "Connection failed".into());
            request_rebuild(&h2);
        }
    });
}

fn aps_band(freq_mhz: Option<u32>) -> u8 {
    crate::nm_client::aps::band_bit(freq_mhz)
}

pub(crate) struct ActiveInfo<'a> {
    pub iface: Option<&'a str>,
    pub ipv4: Option<&'a str>,
    pub gateway: Option<&'a str>,
    pub dns: Option<&'a str>,
    pub freq_mhz: Option<u32>,
}

impl ActiveInfo<'_> {
    pub fn none() -> Self {
        Self {
            iface: None,
            ipv4: None,
            gateway: None,
            dns: None,
            freq_mhz: None,
        }
    }

    fn band_name(freq_mhz: u32) -> &'static str {
        if freq_mhz >= 5925 {
            "6 GHz"
        } else if freq_mhz >= 5000 {
            "5 GHz"
        } else if freq_mhz >= 2400 {
            "2.4 GHz"
        } else {
            ""
        }
    }
}

pub(crate) fn wired_row(info: &crate::state::WiredInfo, h: &UiHandles) -> gtk4::ListBoxRow {
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    hbox.set_margin_start(8);
    hbox.set_margin_end(8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(if info.ipv4.is_some() { 2 } else { 8 });
    let icon =
        gtk4::Image::from_icon_name(&themed_icon(&["network-wired-symbolic", "network-wired"]));
    icon.set_pixel_size(24);
    icon.set_valign(gtk4::Align::Center);
    hbox.append(&icon);
    let name_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    name_box.set_hexpand(true);
    let label = gtk4::Label::new(Some(&info.id));
    label.set_halign(gtk4::Align::Start);
    label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    label.set_max_width_chars(24);
    name_box.append(&label);
    hbox.append(&name_box);
    hbox.append(&action_button(
        "Disconnect",
        true,
        BackendCmd::DisconnectActive,
        h,
    ));
    vbox.append(&hbox);
    if !info.iface.is_empty() {
        let l = detail_line(&format!("Device: {}", info.iface));
        l.set_margin_start(40);
        l.set_margin_end(12);
        vbox.append(&l);
    }
    if let Some(ip) = info.ipv4.as_deref() {
        let l = detail_line(&format!("IPv4: {ip}"));
        l.set_margin_start(40);
        l.set_margin_end(12);
        vbox.append(&l);
    }
    if let Some(mbps) = info.speed_mbps {
        let l = detail_line(&format!("Link: {mbps} Mb/s"));
        l.set_margin_start(40);
        l.set_margin_end(12);
        vbox.append(&l);
    }
    let last = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    last.set_margin_bottom(8);
    vbox.append(&last);
    let row = gtk4::ListBoxRow::new();
    row.set_child(Some(&vbox));
    row.set_activatable(false);
    row.add_css_class("rnet-row");
    row
}

pub const MSG_PASSWORD_SHORT: &str = "Password must be at least 8 characters";
pub const MSG_PASSWORD_LONG: &str = "Password is too long";

pub(crate) fn submit_password(h: &UiHandles, ssid: &str, was_saved: bool) {
    let from_entry: Option<String> = h
        .pw_entries
        .borrow()
        .get(ssid)
        .map(|e| e.text().to_string())
        .filter(|t| !t.is_empty());
    let psk = from_entry
        .or_else(|| h.pw_drafts.borrow().get(ssid).cloned())
        .unwrap_or_default();
    let problem = if psk.len() < 8 {
        Some(MSG_PASSWORD_SHORT)
    } else if psk.len() > 63 {
        Some(MSG_PASSWORD_LONG)
    } else {
        None
    };
    if let Some(problem) = problem {
        h.errors
            .borrow_mut()
            .insert(ssid.to_string(), problem.into());
        request_rebuild(h);
        return;
    }
    h.errors.borrow_mut().remove(ssid);

    let stale_path = h.pending_secret_paths.borrow_mut().remove(ssid);
    let had_path = stale_path.is_some();
    let cmd = if was_saved {
        BackendCmd::RetrySaved {
            ssid: ssid.to_string(),
            psk,
            stale_path,
        }
    } else {
        match stale_path {
            Some(path) => BackendCmd::ProvideSecret {
                ssid: ssid.to_string(),
                path,
                psk,
            },
            None => BackendCmd::ConnectSecure {
                ssid: ssid.to_string(),
                psk,
            },
        }
    };
    mark_connecting(h, ssid, true, !was_saved && !had_path);
    if h.cmd_tx.try_send(cmd).is_err() {
        h.errors
            .borrow_mut()
            .insert(ssid.to_string(), "Backend busy, try again".into());
    }
    *h.expanded.borrow_mut() = None;
    animate_expansion(h, &None);
    set_row_connecting(h, ssid, true);
    h.sync_focus();
}

fn connect_action_button(ap: &Ap, h: &UiHandles) -> gtk4::Button {
    let ssid = ap.ssid.clone();
    let secured = ap.secured;
    let saved = ap.saved;
    let h2 = h.clone();
    action_button_with("Connect", false, move || {
        let typed = h2
            .pw_drafts
            .borrow()
            .get(&ssid)
            .map(|p| (8..=63).contains(&p.len()))
            .unwrap_or(false);
        if secured && typed {
            submit_password(&h2, &ssid, saved);
        } else if !secured {
            mark_connecting(&h2, &ssid, false, false);
            set_row_connecting(&h2, &ssid, true);
            if h2
                .cmd_tx
                .try_send(BackendCmd::ConnectOpen(ssid.clone()))
                .is_err()
            {
                tracing::warn!(%ssid, "connect command dropped: backend channel full");
                set_row_connecting(&h2, &ssid, false);
            }
        } else if saved {
            mark_connecting(&h2, &ssid, false, false);
            set_row_connecting(&h2, &ssid, true);
            if h2
                .cmd_tx
                .try_send(BackendCmd::ConnectSaved(ssid.clone()))
                .is_err()
            {
                tracing::warn!(%ssid, "connect-saved command dropped: backend channel full");
                set_row_connecting(&h2, &ssid, false);
            }
        } else if secured {
            if let Some(entry) = h2.pw_entries.borrow().get(&ssid).cloned() {
                entry.grab_focus();
            }
            return;
        } else {
            mark_connecting(&h2, &ssid, false, false);
            set_row_connecting(&h2, &ssid, true);
            if h2
                .cmd_tx
                .try_send(BackendCmd::ConnectOpen(ssid.clone()))
                .is_err()
            {
                tracing::warn!(%ssid, "connect command dropped: backend channel full");
                set_row_connecting(&h2, &ssid, false);
            }
        }
        h2.sync_focus();
    })
}
fn forget_button(ap: &crate::state::Ap, h: &UiHandles) -> gtk4::Button {
    let ssid = ap.ssid.clone();
    let tx = h.cmd_tx.clone();
    let forget = gtk4::Button::with_label("Forget");
    forget.add_css_class("flat");
    forget.set_tooltip_text(Some("Forget network"));
    forget.connect_clicked(move |_| {
        let _ = tx.try_send(BackendCmd::Forget(ssid.clone()));
    });
    forget
}

fn password_fields(
    ap: &crate::state::Ap,
    h: &UiHandles,
    details: &gtk4::Box,
    _err_label: gtk4::Label,
) -> gtk4::PasswordEntry {
    let entry = gtk4::PasswordEntry::new();
    entry.set_width_chars(12);
    entry.set_show_peek_icon(false);
    entry.set_placeholder_text(Some("Password"));
    if let Some(draft) = h.pw_drafts.borrow().get(&ap.ssid) {
        entry.set_text(draft);
    }
    details.append(&entry);

    {
        let h = h.clone();
        let ssid = ap.ssid.clone();
        let was_saved = ap.saved;
        entry.connect_activate(move |_| submit_password(&h, &ssid, was_saved));
    }
    {
        let h = h.clone();
        let ssid = ap.ssid.clone();
        entry.connect_changed(move |e| {
            let text = e.text();
            h.pw_drafts
                .borrow_mut()
                .insert(ssid.clone(), text.to_string());
            update_connect_sensitivity(&h, &ssid, true);
        });
    }
    entry
}

fn register_pw_fields(h: &UiHandles, ap: &crate::state::Ap, entry: gtk4::PasswordEntry) {
    h.pw_entries.borrow_mut().insert(ap.ssid.clone(), entry);
    update_connect_sensitivity(h, &ap.ssid, true);
}

fn update_connect_sensitivity(h: &UiHandles, ssid: &str, needs_password: bool) {
    let ready = !needs_password
        || h.pw_drafts
            .borrow()
            .get(ssid)
            .map(|p| (8..=63).contains(&p.len()))
            .unwrap_or(false);
    if let Some((btn, _, _)) = h.action_btns.borrow().get(ssid) {
        if h.connecting.borrow().contains_key(ssid) {
            return;
        }
        btn.set_sensitive(ready);
    }
}

pub(crate) fn ap_row(
    ap: &Ap,
    is_active: bool,
    active: ActiveInfo<'_>,
    expanded: bool,
    err: Option<String>,
    h: &UiHandles,
    set_expanded: Rc<dyn Fn(Option<String>)>,
) -> gtk4::ListBoxRow {
    let ActiveInfo {
        iface,
        ipv4,
        gateway,
        dns,
        freq_mhz,
    } = active;
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    hbox.set_margin_start(8);
    hbox.set_margin_end(8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_cursor_from_name(Some("pointer"));

    let vbox_g = vbox.downgrade();
    let ssid = ap.ssid.clone();
    let se = set_expanded.clone();
    let h2 = h.clone();
    let click = gtk4::GestureClick::new();
    click.connect_pressed(move |_, _, x, y| {
        if let Some(vbox_g) = vbox_g.upgrade() {
            let target = vbox_g.pick(x, y, gtk4::PickFlags::DEFAULT);
            let Some(target) = target else { return };
            let mut w: Option<gtk4::Widget> = Some(target);
            while let Some(cur) = w {
                if cur.is::<gtk4::Button>() || cur.is::<gtk4::Entry>() {
                    return;
                }
                w = cur.parent();
            }
        }
        let is_exp = h2.expanded.borrow().as_deref() == Some(ssid.as_str());
        se(if is_exp { None } else { Some(ssid.clone()) });
    });
    vbox.add_controller(click);

    let (sig, set_strength) = net_icon_live(ap.strength, ap.secured);
    hbox.append(&sig);
    h.strength_setters
        .borrow_mut()
        .insert(ap.ssid.clone(), set_strength);

    let name_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    name_box.set_hexpand(true);
    let ssid = gtk4::Label::new(Some(&ap.ssid));
    ssid.set_halign(gtk4::Align::Start);
    ssid.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    ssid.set_max_width_chars(SSID_CHARS);
    h.ssid_labels
        .borrow_mut()
        .insert(ap.ssid.clone(), ssid.clone());
    name_box.append(&ssid);

    let add_badge = |txt: &str| {
        let extra = gtk4::Label::new(Some(txt));
        extra.add_css_class("dim-label");
        name_box.append(&extra);
    };
    let add_chip = |txt: &str| {
        let chip = gtk4::Label::new(Some(txt));
        chip.add_css_class("rnet-chip");
        chip.set_valign(gtk4::Align::Center);
        name_box.append(&chip);
    };
    let bands = if is_active {
        ap.bands | aps_band(freq_mhz.or(ap.freq_mhz))
    } else {
        ap.bands
    };
    let band_label = crate::nm_client::aps::band_label(bands);
    if !band_label.is_empty() {
        add_chip(&band_label);
    }
    if ap.saved && !is_active {
        let saved = gtk4::Label::new(Some("saved"));
        saved.add_css_class("rnet-chip-saved");
        saved.set_valign(gtk4::Align::Center);
        saved.set_tooltip_text(Some("Saved network"));
        name_box.append(&saved);
    }
    hbox.append(&name_box);

    let is_connecting = !is_active && h.connecting.borrow().contains_key(&ap.ssid);
    {
        let spin = gtk4::Spinner::new();
        spin.set_size_request(16, 16);
        if is_connecting {
            spin.start();
        }
        let l = gtk4::Label::new(Some("Connecting…"));
        l.add_css_class("dim-label");
        let status = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        status.append(&spin);
        status.append(&l);
        status.set_visible(is_connecting);
        h.status
            .borrow_mut()
            .insert(ap.ssid.clone(), status.clone());
        hbox.append(&status);
    }

    let card_actions = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    card_actions.set_halign(gtk4::Align::End);
    h.card_actions
        .borrow_mut()
        .insert(ap.ssid.clone(), card_actions.clone());

    if is_active {
        hbox.append(&action_button(
            "Disconnect",
            true,
            BackendCmd::DisconnectActive,
            h,
        ));
    } else {
        let action = connect_action_button(ap, h);
        let slot = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        h.action_btns.borrow_mut().insert(
            ap.ssid.clone(),
            (action.clone(), hbox.clone(), slot.clone()),
        );
        hbox.append(&slot);
        if expanded && ap.secured {
            card_actions.append(&action);
        } else {
            hbox.append(&action);
        }
    }
    if ap.enterprise || ap.wep {
        add_badge(if ap.enterprise { "enterprise" } else { "WEP" });
        let cfg = gtk4::Button::with_label("Configure");
        cfg.add_css_class("flat");
        cfg.set_tooltip_text(Some("Open connection settings"));
        cfg.connect_clicked(move |_| super::placement::open_editor());
        hbox.append(&cfg);
    }

    {
        let ssid = ap.ssid.clone();
        let se = set_expanded.clone();
        let h2 = h.clone();
        let chev_img = gtk4::Image::from_icon_name(&themed_icon(&[if expanded {
            "pan-up-symbolic"
        } else {
            "pan-down-symbolic"
        }]));
        chev_img.set_pixel_size(16);
        let chev = gtk4::Button::new();
        chev.set_child(Some(&chev_img));
        chev.add_css_class("flat");
        chev.set_tooltip_text(Some("Details"));
        chev.set_focusable(false);
        chev.set_focus_on_click(false);
        chev.connect_clicked(move |_| {
            let is_exp = h2.expanded.borrow().as_deref() == Some(ssid.as_str());
            se(if is_exp { None } else { Some(ssid.clone()) });
        });
        h.chevrons.borrow_mut().insert(ap.ssid.clone(), chev_img);
        hbox.append(&chev);
    }
    vbox.append(&hbox);

    let details = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    details.set_margin_start(40);
    details.set_margin_end(12);
    details.set_margin_bottom(10);
    let has_err = err.is_some();

    if is_active {
        if let Some(dev) = iface {
            details.append(&detail_line(&format!("Device: {dev}")));
        }
        if let Some(ip) = ipv4 {
            details.append(&detail_line(&format!("IPv4: {ip}")));
        }
        if let Some(gw) = gateway {
            details.append(&detail_line(&format!("Gateway: {gw}")));
        }
        if let Some(dns) = dns {
            details.append(&detail_line(&format!("DNS: {dns}")));
        }
        if let Some(f) = freq_mhz.or(ap.freq_mhz) {
            let band = ActiveInfo::band_name(f);
            if !band.is_empty() {
                details.append(&detail_line(&format!("Band: {band}")));
            }
        }
        let (up, down) = *h.speeds.borrow();
        let speed_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 16);
        let down_label = detail_line(&format!("↓ {}", crate::state::format_rate(down)));
        let up_label = detail_line(&format!("↑ {}", crate::state::format_rate(up)));
        speed_row.append(&down_label);
        speed_row.append(&up_label);
        details.append(&speed_row);
        *h.speed_labels.borrow_mut() = Some((down_label, up_label));
    } else if !ap.secured {
        let l = gtk4::Label::new(Some("Open network"));
        l.set_halign(gtk4::Align::Start);
        l.add_css_class("dim-label");
        details.append(&l);
        if ap.saved {
            card_actions.append(&forget_button(ap, h));
        }
        details.append(&card_actions);
    } else if ap.enterprise || ap.wep {
        if let Some(e) = err.as_deref() {
            let el = gtk4::Label::new(Some(e));
            el.set_halign(gtk4::Align::Start);
            el.add_css_class("error");
            el.set_wrap(true);
            details.append(&el);
        }
        let hint = if ap.enterprise {
            "Enterprise network — certificates are configured in Network settings"
        } else {
            "WEP network — configure the key in Network settings"
        };
        let l = gtk4::Label::new(Some(hint));
        l.set_halign(gtk4::Align::Start);
        l.add_css_class("dim-label");
        l.set_wrap(true);
        details.append(&l);
        details.append(&card_actions);
    } else {
        let err_label = {
            let el = gtk4::Label::new(Some(err.as_deref().unwrap_or("")));
            el.set_halign(gtk4::Align::Start);
            el.add_css_class("error");
            el.set_wrap(true);
            el.set_visible(err.is_some());
            details.append(&el);
            h.error_labels
                .borrow_mut()
                .insert(ap.ssid.clone(), el.clone());
            el
        };
        let show_pw = ap.secured && (!ap.saved || has_err);
        if show_pw {
            let entry = password_fields(ap, h, &details, err_label);
            register_pw_fields(h, ap, entry);
        }
        if ap.saved {
            card_actions.append(&forget_button(ap, h));
        }
        details.append(&card_actions);
    }

    let revealer = gtk4::Revealer::new();
    revealer.set_transition_type(gtk4::RevealerTransitionType::SlideDown);
    revealer.set_transition_duration(crate::ui::motion::REVEAL_MS);
    revealer.set_child(Some(&details));
    revealer.set_reveal_child(expanded);
    h.revealers
        .borrow_mut()
        .insert(ap.ssid.clone(), revealer.clone());
    vbox.append(&revealer);

    let row = gtk4::ListBoxRow::new();
    row.set_child(Some(&vbox));
    row.set_activatable(false);
    row.add_css_class("rnet-row");
    row
}
