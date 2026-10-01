use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::prelude::*;

use super::list::{refresh_list, request_rebuild};
use super::state::UiHandles;
use super::theme::{net_icon_live, themed_icon};
use crate::state::{Ap, BackendCmd};

const ERROR_SHOW_FOR: Duration = Duration::from_secs(12);
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
    let btn = gtk4::Button::new();
    btn.set_child(Some(&img));
    btn.add_css_class("flat");
    btn.set_valign(gtk4::Align::Center);
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
    let tx = h.cmd_tx.clone();
    let h2 = h.clone();
    let cmd2 = cmd.clone();
    btn.connect_clicked(move |_| {
        if let Some(ssid) = target_ssid(&cmd2) {
            mark_connecting(&h2, &ssid, false, false);
        }
        let _ = tx.try_send(cmd2.clone());
    });
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
    let before: Vec<(String, bool)> = h
        .revealers
        .borrow()
        .iter()
        .map(|(k, v)| (k.clone(), v.is_child_revealed()))
        .collect();
    let starts: Vec<(String, bool, gtk4::graphene::Rect)> = before
        .iter()
        .filter(|(ssid, was)| (target == Some(ssid.as_str())) != *was)
        .filter_map(|(ssid, was)| {
            let now = target == Some(ssid.as_str());
            crate::ui::motion::measure(h, ssid, *was).map(|r| (ssid.clone(), now, r))
        })
        .collect();
    for (ssid, rev) in h.revealers.borrow().iter() {
        let open = target == Some(ssid.as_str());
        rev.set_transition_duration(crate::ui::motion::duration(open));
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
    for (ssid, btn) in h.connect_btns.borrow().iter() {
        let open = target == Some(ssid.as_str());
        if open {
            btn.set_visible(false);
        } else {
            btn.set_visible(true);
            btn.set_opacity(1.0);
        }
    }
    for (ssid, l) in h.ssid_labels.borrow().iter() {
        if target == Some(ssid.as_str()) {
            l.set_ellipsize(gtk4::pango::EllipsizeMode::None);
            l.set_max_width_chars(18);
        } else {
            l.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            l.set_max_width_chars(18);
        }
    }
    for (ssid, now_open, start) in starts {
        crate::ui::motion::run(h, &ssid, now_open, start);
    }
    if let Some(ssid) = target {
        let rev = h.revealers.borrow().get(ssid).cloned();
        let entry = h.pw_entries.borrow().get(ssid).cloned();
        if let (Some(rev), Some(entry)) = (rev, entry) {
            let done = std::cell::Cell::new(false);
            rev.connect_child_revealed_notify(move |r| {
                if done.get() || !r.is_child_revealed() {
                    return;
                }
                done.set(true);
                if entry.is_mapped() {
                    entry.grab_focus();
                }
            });
        }
    }
}

pub(crate) fn expander(h: &UiHandles) -> Rc<dyn Fn(Option<String>)> {
    let h = h.clone();
    Rc::new(move |v| {
        let collapsing = v.is_none();
        *h.focus_ssid.borrow_mut() = v.clone();
        *h.expanded.borrow_mut() = v.clone();
        animate_expansion(&h, &v);
        if collapsing {
            request_rebuild(&h);
        }
    })
}

pub(crate) fn flash_error(h: &UiHandles, ssid: &str, message: &str) {
    let deadline = Instant::now() + ERROR_SHOW_FOR;
    h.errors
        .borrow_mut()
        .insert(ssid.to_string(), message.to_string());
    h.err_token.borrow_mut().insert(ssid.to_string(), deadline);
    request_rebuild(h);
    let h2 = h.clone();
    let ssid = ssid.to_string();
    gtk4::glib::timeout_add_local_once(ERROR_SHOW_FOR, move || {
        if h2.err_token.borrow().get(&ssid) == Some(&deadline) {
            h2.err_token.borrow_mut().remove(&ssid);
            h2.errors.borrow_mut().remove(&ssid);
            request_rebuild(&h2);
        }
    });
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

pub(crate) struct ActiveInfo<'a> {
    pub iface: Option<&'a str>,
    pub ipv4: Option<&'a str>,
}

impl ActiveInfo<'_> {
    pub fn none() -> Self {
        Self {
            iface: None,
            ipv4: None,
        }
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
    let ActiveInfo { iface, ipv4 } = active;
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
    ssid.set_max_width_chars(18);
    h.ssid_labels
        .borrow_mut()
        .insert(ap.ssid.clone(), ssid.clone());
    name_box.append(&ssid);

    let add_badge = |txt: &str| {
        let extra = gtk4::Label::new(Some(txt));
        extra.add_css_class("dim-label");
        name_box.append(&extra);
    };
    if is_active {
        match iface {
            Some(i) => add_badge(&format!("({i})")),
            None if ap.saved => add_badge("(saved)"),
            None => {}
        }
    } else if ap.saved {
        add_badge("saved");
    }
    hbox.append(&name_box);

    let is_connecting = !is_active && h.connecting.borrow().contains_key(&ap.ssid);
    if is_connecting {
        let spin = gtk4::Spinner::new();
        spin.set_size_request(16, 16);
        spin.start();
        hbox.append(&spin);
        let l = gtk4::Label::new(Some("Connecting…"));
        l.add_css_class("dim-label");
        hbox.append(&l);
    }

    if is_active {
        hbox.append(&action_button(
            "Disconnect",
            true,
            BackendCmd::DisconnectActive,
            h,
        ));
    } else if !ap.secured || ap.saved {
        if !(expanded && err.is_some()) {
            let cmd = if ap.saved {
                BackendCmd::ConnectSaved(ap.ssid.clone())
            } else {
                BackendCmd::ConnectOpen(ap.ssid.clone())
            };
            hbox.append(&action_button("Connect", false, cmd, h));
        }
    } else {
        let connect = gtk4::Button::with_label("Connect");
        connect.add_css_class("flat");
        connect.set_halign(gtk4::Align::End);
        let ssid = ap.ssid.clone();
        let se = set_expanded.clone();
        connect.set_visible(!expanded);
        h.connect_btns
            .borrow_mut()
            .insert(ap.ssid.clone(), connect.clone());
        connect.connect_clicked(move |_| se(Some(ssid.clone())));
        hbox.append(&connect);
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
        if let Some(ip) = ipv4 {
            details.append(&detail_line(&format!("IPv4: {ip}")));
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
    } else if ap.saved && !has_err {
        let l = gtk4::Label::new(Some("Saved"));
        l.set_halign(gtk4::Align::Start);
        l.add_css_class("dim-label");
        details.append(&l);
    } else {
        let err_label = err.map(|e| {
            let el = gtk4::Label::new(Some(&e));
            el.set_halign(gtk4::Align::Start);
            el.add_css_class("error");
            el.set_wrap(true);
            details.append(&el);
            el
        });
        let entry = gtk4::PasswordEntry::new();
        entry.set_width_chars(12);
        entry.set_show_peek_icon(true);
        entry.set_placeholder_text(Some("Password"));
        if let Some(draft) = h.pw_drafts.borrow().get(&ap.ssid) {
            entry.set_text(draft);
        }
        details.append(&entry);

        let submit = gtk4::Button::with_label("Connect");
        submit.set_halign(gtk4::Align::End);
        submit.set_sensitive((8..=63).contains(&entry.text().len()));
        details.append(&submit);

        {
            let h_motion = h.clone();
            let motion = gtk4::EventControllerMotion::new();
            motion.connect_enter({
                let h = h_motion.clone();
                move |_, _, _| h.sync_hover(true)
            });
            motion.connect_leave({
                let h = h_motion.clone();
                move |_| h.sync_hover(false)
            });
            entry.add_controller(motion);
        }
        {
            let submit_w = submit.downgrade();
            let ssid = ap.ssid.clone();
            let h = h.clone();
            entry.connect_changed(move |e| {
                let Some(submit) = submit_w.upgrade() else {
                    return;
                };
                submit.set_sensitive((8..=63).contains(&e.text().len()));
                h.pw_drafts
                    .borrow_mut()
                    .insert(ssid.clone(), e.text().to_string());
                if !e.text().is_empty() {
                    h.errors.borrow_mut().remove(&ssid);
                    h.err_token.borrow_mut().remove(&ssid);
                    if let Some(el) = err_label.as_ref() {
                        el.set_visible(false);
                    }
                }
            });
        }

        let do_submit: Rc<dyn Fn()> = Rc::new({
            let entry_w = entry.downgrade();
            let ssid = ap.ssid.clone();
            let was_saved = ap.saved;
            let h = h.clone();
            move || {
                let Some(entry) = entry_w.upgrade() else {
                    return;
                };
                let psk = entry.text().to_string();
                if !(8..=63).contains(&psk.len()) {
                    h.errors
                        .borrow_mut()
                        .insert(ssid.clone(), "Password must be 8-63 characters".into());
                    request_rebuild(&h);
                    return;
                }
                h.errors.borrow_mut().remove(&ssid);
                let had_path = h.pending_secret_paths.borrow().contains_key(&ssid);
                let cmd = match h.pending_secret_paths.borrow_mut().remove(&ssid) {
                    Some(path) => BackendCmd::ProvideSecret { path, psk },
                    None => BackendCmd::ConnectSecure {
                        ssid: ssid.clone(),
                        psk,
                    },
                };
                mark_connecting(&h, &ssid, true, !was_saved && !had_path);
                let _ = h.cmd_tx.try_send(cmd);
                h.pw_drafts.borrow_mut().remove(&ssid);
                entry.set_text("");
                refresh_list(&h);
            }
        });
        {
            let do_submit = do_submit.clone();
            submit.connect_clicked(move |_| do_submit());
        }
        {
            let do_submit = do_submit.clone();
            entry.connect_activate(move |_| do_submit());
        }
        h.pw_entries.borrow_mut().insert(ap.ssid.clone(), entry);
        h.submits
            .borrow_mut()
            .insert(ap.ssid.clone(), submit.clone());
    }

    if ap.saved && !is_active && !has_err {
        let ssid = ap.ssid.clone();
        let tx = h.cmd_tx.clone();
        let forget = gtk4::Button::with_label("Forget");
        forget.add_css_class("flat");
        forget.set_halign(gtk4::Align::End);
        forget.set_tooltip_text(Some("Forget network"));
        forget.connect_clicked(move |_| {
            let _ = tx.try_send(BackendCmd::Forget(ssid.clone()));
        });
        details.append(&forget);
    }

    let revealer = gtk4::Revealer::new();
    revealer.set_transition_type(gtk4::RevealerTransitionType::SlideDown);
    revealer.set_transition_duration(crate::ui::motion::FLIP_MS);
    revealer.set_child(Some(&details));
    revealer.set_reveal_child(expanded);
    h.revealers
        .borrow_mut()
        .insert(ap.ssid.clone(), revealer.clone());
    vbox.append(&revealer);

    let flips = !is_active && ap.secured && !ap.saved;
    let row = gtk4::ListBoxRow::new();
    if flips {
        let overlay = crate::ui::motion::overlay_for(h, &ap.ssid, &vbox, "Connect");
        row.set_child(Some(&overlay));
    } else {
        row.set_child(Some(&vbox));
    }
    row.set_activatable(false);
    row.add_css_class("rnet-row");
    row
}
