use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk4::cairo;
use gtk4::gdk::Key;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::state::{Ap, BackendCmd, Model, UiEvent};

const POPUP_W: i32 = 426;

const CSS: &str = "
window.rnet-window, window.rnet-window > .background, window.rnet-window:backdrop, window.rnet-window:backdrop > .background, window.rnet-window.background, window.rnet-window.background:backdrop { background-color: transparent; background-image: none; box-shadow: none; border: none; border-radius: 0; }
.rnet-popup { opacity: 0; transition: opacity 120ms ease-out; }
.rnet-popup.open { opacity: 0.99; }
.rnet-card { background-color: @theme_bg_color; border-radius: 12px; padding: 8px 6px; }
.rnet-window image { opacity: 1; }
.rnet-title { margin: 0; }
.rnet-section-row { background-color: transparent; }
.rnet-section-row:hover, .rnet-section-row:active, .rnet-section-row:selected { background-color: transparent; }
.rnet-row { min-height: 48px; margin: 1px 0; border-radius: 4px; transition: background-color 120ms ease-in-out; }
.flat { transition: background-color 120ms ease-in-out; }
button.suggested-action { transition: background-color 120ms ease-in-out; }
.rnet-popover { padding: 14px; }
.rnet-hotspot-key { font-family: monospace; }
.rnet-status { padding-top: 6px; }
.error { color: @error_color; }
.disconnect-label { color: @error_color; }
.rnet-row-hover:hover { background-color: alpha(@theme_fg_color, 0.08); }
.rnet-row-hover:active { background-color: alpha(@theme_fg_color, 0.13); }
.rnet-qr { padding-left: 10px; padding-right: 10px; }
.rnet-toolbar entry { min-height: 30px; }
.rnet-lock-badge { background-color: @theme_base_color; border-radius: 999px; padding: 0.4px; }
.rnet-lock-overlay { margin-left: -10px; margin-top: 10px; }
";

pub fn filter_aps(aps: &[Ap], query: &str) -> Vec<Ap> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return aps.to_vec();
    }
    aps.iter()
        .filter(|ap| ap.ssid.to_lowercase().contains(&q))
        .cloned()
        .collect()
}

#[allow(dead_code)]
pub fn signal_icon_name(strength: u8) -> &'static str {
    match strength {
        0..=5 => "network-wireless-signal-none-symbolic",
        6..=30 => "network-wireless-signal-weak-symbolic",
        31..=55 => "network-wireless-signal-ok-symbolic",
        56..=80 => "network-wireless-signal-good-symbolic",
        _ => "network-wireless-signal-excellent-symbolic",
    }
}

fn signal_bars(strength: u8) -> gtk4::DrawingArea {
    let filled = match strength {
        0..=5 => 0,
        6..=30 => 1,
        31..=55 => 2,
        56..=80 => 3,
        _ => 4,
    };
    let area = gtk4::DrawingArea::new();
    area.set_content_width(24);
    area.set_content_height(24);
    area.set_valign(gtk4::Align::Center);
    area.set_draw_func(move |w, cr, _width, _height| {
        let fg = w.color();
        let faint = gtk4::gdk::RGBA::new(
            fg.red(),
            fg.green(),
            fg.blue(),
            (fg.alpha() * 0.22).min(1.0),
        );
        for i in 0..4 {
            let c = if i < filled { fg } else { faint };
            cr.set_source_rgba(
                c.red() as f64,
                c.green() as f64,
                c.blue() as f64,
                c.alpha() as f64,
            );
            let x = 1.0 + i as f64 * 6.0;
            let h = 6.0 + i as f64 * 4.0;
            rounded_rect(cr, x, 22.0 - h, 4.2, h, 1.2);
            let _ = cr.fill();
        }
    });
    area
}

fn rounded_rect(
    cr: &cairo::Context,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    r: f64,
) {
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, PI + FRAC_PI_2);
    cr.close_path();
}

thread_local! {
    static THEMED_CACHE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static LOOKUP_CACHE: RefCell<HashMap<(String, i32), bool>> = RefCell::new(HashMap::new());
}

fn lookup_ok(name: &str, size: i32) -> bool {
    let key = (name.to_string(), size);
    LOOKUP_CACHE.with(|c| {
        if let Some(v) = c.borrow().get(&key) {
            return *v;
        }
        let v = gtk4::gdk::Display::default()
            .map(|d| {
                gtk4::IconTheme::for_display(&d)
                    .lookup_icon(
                        name,
                        &[],
                        size,
                        1,
                        gtk4::TextDirection::Ltr,
                        gtk4::IconLookupFlags::empty(),
                    )
                    .file()
                    .is_some()
            })
            .unwrap_or(false);
        c.borrow_mut().insert(key, v);
        v
    })
}

fn lock_emblem() -> gtk4::Image {
    let lock = gtk4::Image::from_icon_name(&themed_icon(&[
        "lock-symbolic",
        "network-wireless-encrypted-symbolic",
        "system-lock-screen-symbolic",
    ]));
    lock.set_pixel_size(10);
    lock.add_css_class("rnet-lock-badge");
    lock.set_halign(gtk4::Align::End);
    lock.set_valign(gtk4::Align::End);
    lock.set_margin_end(1);
    lock
}

fn bars_with_lock(strength: u8) -> gtk4::Widget {
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&signal_bars(strength)));
    overlay.add_overlay(&lock_emblem());
    overlay.set_size_request(24, 24);
    overlay.upcast()
}

fn signal_step(strength: u8) -> &'static str {
    match strength {
        0..=5 => "none",
        6..=30 => "weak",
        31..=55 => "ok",
        56..=80 => "good",
        _ => "excellent",
    }
}

fn centered_image(name: &str) -> gtk4::Image {
    let img = gtk4::Image::from_icon_name(name);
    img.set_pixel_size(24);
    img.set_valign(gtk4::Align::Center);
    img.set_halign(gtk4::Align::Center);
    img
}

fn papirus_theme() -> bool {
    gtk4::Settings::default()
        .and_then(|s| s.gtk_icon_theme_name())
        .map(|n| n.as_str().to_lowercase().contains("papirus"))
        .unwrap_or(false)
}

fn net_icon(strength: u8, secured: bool) -> gtk4::Widget {
    if papirus_theme() {
        if !secured {
            return signal_bars(strength).upcast();
        }
        return bars_with_lock(strength);
    }
    let step = signal_step(strength);
    let sym = format!("network-wireless-signal-{step}-symbolic");
    let plain = format!("network-wireless-signal-{step}");
    let base = if lookup_ok(&sym, 24) {
        sym
    } else if lookup_ok(&plain, 24) {
        plain
    } else {
        String::new()
    };
    if base.is_empty() {
        if secured {
            return bars_with_lock(strength);
        }
        return signal_bars(strength).upcast();
    }
    if !secured {
        return centered_image(&base).upcast();
    }
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&centered_image(&base)));
    overlay.add_overlay(&lock_emblem());
    overlay.set_size_request(24, 24);
    overlay.upcast()
}

pub(crate) fn load_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(CSS);
    let Some(display) = gtk4::gdk::Display::default() else {
        tracing::warn!("no display; skipping applet CSS");
        return;
    };
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_USER,
    );
}

fn hide_popup(
    window: &gtk4::ApplicationWindow,
    revealer: &gtk4::Revealer,
    visible: &Rc<Cell<bool>>,
    hide_gen: &Rc<Cell<u64>>,
) {
    visible.set(false);
    revealer.remove_css_class("open");
    revealer.set_reveal_child(false);
    let g = hide_gen.get() + 1;
    hide_gen.set(g);
    let seen = hide_gen.clone();
    let window = window.clone();
    gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
        if seen.get() == g {
            window.set_visible(false);
        }
    });
}

fn open_editor() {
    if let Err(e) = std::process::Command::new("nm-connection-editor").spawn() {
        tracing::warn!("could not launch nm-connection-editor: {e}");
    }
}

#[derive(Clone)]
struct UiHandles {
    list: gtk4::ListBox,
    scroll: gtk4::ScrolledWindow,
    model: Rc<RefCell<Model>>,
    query: Rc<RefCell<String>>,

    expanded: Rc<RefCell<Option<String>>>,

    errors: Rc<RefCell<HashMap<String, String>>>,

    err_token: Rc<RefCell<HashMap<String, std::time::Instant>>>,

    focus_ssid: Rc<RefCell<Option<String>>>,

    pw_drafts: Rc<RefCell<HashMap<String, String>>>,

    pending_secret_paths: Rc<RefCell<HashMap<String, String>>>,

    connecting: Rc<RefCell<HashMap<String, std::time::Instant>>>,

    pw_attempt: Rc<RefCell<std::collections::HashSet<String>>>,

    unsaved_attempt: Rc<RefCell<std::collections::HashSet<String>>>,

    hidden_expanded: Rc<Cell<bool>>,

    hidden_closing: Rc<Cell<bool>>,

    hidden_card: Rc<RefCell<Option<(gtk4::Revealer, gtk4::ListBoxRow)>>>,

    hidden_error: Rc<RefCell<Option<String>>>,

    hotspot_error: Rc<RefCell<Option<String>>>,

    hotspot_expanded: Rc<Cell<bool>>,

    hotspot_was: Rc<Cell<bool>>,

    hotspot_ssid: Rc<RefCell<String>>,
    hotspot_psk: Rc<RefCell<String>>,
    speeds: Rc<RefCell<(Option<u64>, Option<u64>)>>,
    speed_labels: Rc<RefCell<Option<(gtk4::Label, gtk4::Label)>>>,
    revealers: Rc<RefCell<HashMap<String, gtk4::Revealer>>>,
    chevrons: Rc<RefCell<HashMap<String, gtk4::Image>>>,
    connect_btns: Rc<RefCell<HashMap<String, gtk4::Button>>>,
    ssid_labels: Rc<RefCell<HashMap<String, gtk4::Label>>>,
    pw_entries: Rc<RefCell<HashMap<String, gtk4::PasswordEntry>>>,
    cmd_tx: async_channel::Sender<BackendCmd>,
}

fn animated_card(rows: Vec<gtk4::Widget>) -> gtk4::ListBoxRow {
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
    let rev2 = rev.clone();
    gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(8), move || {
        rev2.set_reveal_child(true);
    });
    let wrap = gtk4::ListBoxRow::new();
    wrap.set_activatable(false);
    wrap.set_selectable(false);
    wrap.set_child(Some(&rev));
    wrap
}

fn expander(h: &UiHandles) -> Rc<dyn Fn(Option<String>)> {
    let h = h.clone();
    Rc::new(move |v| {
        *h.focus_ssid.borrow_mut() = v.clone();
        *h.expanded.borrow_mut() = v.clone();
        animate_expansion(&h, &v);
    })
}

fn animate_expansion(h: &UiHandles, target: &Option<String>) {
    let target = target.as_deref();
    {
        let reveals = h.revealers.borrow();
        for (ssid, rev) in reveals.iter() {
            let open = target == Some(ssid.as_str());
            rev.set_transition_duration(if open { 160 } else { 90 });
            rev.set_reveal_child(open);
        }
    }
    {
        let chevrons = h.chevrons.borrow();
        for (ssid, img) in chevrons.iter() {
            let name = if target == Some(ssid.as_str()) {
                "pan-up-symbolic"
            } else {
                "pan-down-symbolic"
            };
            img.set_icon_name(Some(&themed_icon(&[name])));
        }
    }
    if let Some(ssid) = target {
        let btns = h.connect_btns.borrow();
        if let Some(btn) = btns.get(ssid) {
            btn.set_visible(false);
        }
    }
    {
        let labels = h.ssid_labels.borrow();
        for (ssid, l) in labels.iter() {
            if target == Some(ssid.as_str()) {
                l.set_ellipsize(gtk4::pango::EllipsizeMode::None);
                l.set_max_width_chars(200);
            } else {
                l.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                l.set_max_width_chars(21);
            }
        }
    }
    if let Some(ssid) = target {
        if let Some(entry) = h.pw_entries.borrow().get(ssid) {
            entry.grab_focus();
        }
    }
}

fn themed_icon(candidates: &[&str]) -> String {
    let key = candidates.join("\u{1}");
    THEMED_CACHE.with(|c| {
        if let Some(v) = c.borrow().get(&key) {
            return v.clone();
        }
        let has = |n: &str| {
            gtk4::gdk::Display::default()
                .map(|d| gtk4::IconTheme::for_display(&d).has_icon(n))
                .unwrap_or(false)
        };
        let v = candidates
            .iter()
            .find(|n| has(n))
            .or_else(|| candidates.first())
            .unwrap_or(&"network-wireless-symbolic")
            .to_string();
        c.borrow_mut().insert(key, v.clone());
        v
    })
}

fn place_near(revealer: &gtk4::Revealer, x: i32, _y: i32) {
    let mon_w = tray_monitor_width(x).unwrap_or(1920);
    let right = (mon_w - x - POPUP_W / 2).clamp(2, (mon_w - POPUP_W - 2).max(2));
    revealer.set_margin_end(right);
    revealer.set_margin_top(0);
}

fn tray_monitor_width(x: i32) -> Option<i32> {
    let display = gtk4::gdk::Display::default()?;
    let monitors = display.monitors();
    let mut rightmost: Option<(i32, i32)> = None;
    for i in 0..monitors.n_items() {
        if let Some(obj) = monitors.item(i) {
            if let Ok(m) = obj.downcast::<gtk4::gdk::Monitor>() {
                let g = m.geometry();
                if x >= g.x() && x <= g.x() + g.width() {
                    return Some(g.width());
                }
                match rightmost {
                    Some((rx, _)) if g.x() > rx => rightmost = Some((g.x(), g.width())),
                    None => rightmost = Some((g.x(), g.width())),
                    _ => {}
                }
            }
        }
    }
    rightmost.map(|(_, w)| w)
}

fn flat_icon_button(icon: &str, pixel: i32, tooltip: &str) -> gtk4::Button {
    let img = gtk4::Image::from_icon_name(&themed_icon(&[icon]));
    img.set_pixel_size(pixel);
    let btn = gtk4::Button::new();
    btn.set_child(Some(&img));
    btn.add_css_class("flat");
    btn.set_tooltip_text(Some(tooltip));
    btn.set_focusable(false);
    btn.set_focus_on_click(false);
    btn
}

fn action_button(label: &str, connected: bool, cmd: BackendCmd, h: &UiHandles) -> gtk4::Button {
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
        match &cmd2 {
            BackendCmd::ConnectOpen(ssid) | BackendCmd::Forget(ssid) => {
                if !matches!(&cmd2, BackendCmd::Forget(_)) {
                    mark_connecting(&h2, ssid, false, false);
                }
                let _ = tx.try_send(cmd2.clone());
            }
            BackendCmd::ConnectSaved(ssid) => {
                mark_connecting(&h2, ssid, false, false);
                let _ = tx.try_send(cmd2.clone());
            }
            BackendCmd::ConnectSecure { ssid, .. } => {

                mark_connecting(&h2, ssid, false, false);
                let _ = tx.try_send(cmd2.clone());
            }
            _ => {
                let _ = tx.try_send(cmd2.clone());
            }
        }
    });
    btn
}

fn ap_row(
    ap: &Ap,
    is_active: bool,
    iface: Option<&str>,
    ipv4: Option<&str>,
    expanded: bool,
    err: Option<String>,
    h: &UiHandles,
    set_expanded: Rc<dyn Fn(Option<String>)>,
) -> gtk4::ListBoxRow {
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    hbox.set_margin_start(8);
    hbox.set_margin_end(8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_cursor_from_name(Some("pointer"));

    {
        let vbox_g = vbox.clone();
        let ssid = ap.ssid.clone();
        let se = set_expanded.clone();
        let h2 = h.clone();
        let click = gtk4::GestureClick::new();
        click.connect_pressed(move |_, _, x, y| {
            if let Some(target) = vbox_g.pick(x, y, gtk4::PickFlags::DEFAULT) {
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
    }

    let sig = net_icon(ap.strength, ap.secured);
    hbox.append(&sig);

    let name_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    name_box.set_hexpand(true);
    let ssid = gtk4::Label::new(Some(&ap.ssid));
    ssid.set_halign(gtk4::Align::Start);
    if expanded {
        ssid.set_ellipsize(gtk4::pango::EllipsizeMode::None);
        ssid.set_max_width_chars(200);
    } else {
        ssid.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        ssid.set_max_width_chars(21);
    }
    h.ssid_labels.borrow_mut().insert(ap.ssid.clone(), ssid.clone());
    name_box.append(&ssid);
    if is_active {
        if let Some(iface) = iface {
            let extra = gtk4::Label::new(Some(&format!("({iface})")));
            extra.add_css_class("dim-label");
            name_box.append(&extra);
        } else if ap.saved {
            let extra = gtk4::Label::new(Some("(saved)"));
            extra.add_css_class("dim-label");
            name_box.append(&extra);
        }
    } else if ap.saved {
        let extra = gtk4::Label::new(Some("saved"));
        extra.add_css_class("dim-label");
        name_box.append(&extra);
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

        let retrying = expanded && err.is_some();
        if !retrying {
            let cmd = if ap.saved {
                BackendCmd::ConnectSaved(ap.ssid.clone())
            } else {
                BackendCmd::ConnectOpen(ap.ssid.clone())
            };
            hbox.append(&action_button("Connect", false, cmd, h));
        }
    } else if !expanded {

        let connect = gtk4::Button::with_label("Connect");
        let connect2 = connect.clone();
        connect.add_css_class("flat");
        connect.set_halign(gtk4::Align::End);
        let ssid = ap.ssid.clone();
        let se = set_expanded.clone();
        h.connect_btns
            .borrow_mut()
            .insert(ap.ssid.clone(), connect.clone());
        connect.connect_clicked(move |_| {
            connect2.set_visible(false);
            se(Some(ssid.clone()));
        });
        hbox.append(&connect);
    }

    {
        let ssid = ap.ssid.clone();
        let se = set_expanded.clone();
        let h2 = h.clone();
        let chev_img = gtk4::Image::from_icon_name(&themed_icon(&[
            if expanded { "pan-up-symbolic" } else { "pan-down-symbolic" },
        ]));
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
            let (up, down) = h.speeds.borrow().clone();
            let speed_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 16);
            let down_label = detail_line(&format!(
                "↓ {}",
                crate::state::format_rate(down)
            ));
            let up_label = detail_line(&format!(
                "↑ {}",
                crate::state::format_rate(up)
            ));
            speed_row.append(&down_label);
            speed_row.append(&up_label);
            details.append(&speed_row);
            *h.speed_labels.borrow_mut() = Some((down_label, up_label));
        } else if !ap.secured {
            let l = gtk4::Label::new(Some("Open network"));
            l.set_halign(gtk4::Align::Start);
            l.add_css_class("dim-label");
            details.append(&l);
        } else if ap.saved && err.is_none() {
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
            submit.add_css_class("suggested-action");

            submit.set_sensitive(entry.text().len() >= 8);
            details.append(&submit);

            {
                let ssid = ap.ssid.clone();
                let h = h.clone();
                entry.connect_has_focus_notify(move |e| {
                    if e.has_focus() {
                        *h.focus_ssid.borrow_mut() = Some(ssid.clone());
                    } else if h.focus_ssid.borrow().as_deref() == Some(ssid.as_str()) {
                        h.focus_ssid.borrow_mut().take();
                    }
                });
            }
            {
                let submit = submit.clone();
                let ssid = ap.ssid.clone();
                let h = h.clone();
                entry.connect_changed(move |e| {
                    submit.set_sensitive(e.text().len() >= 8);
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

            let do_submit = Rc::new({
                let entry = entry.clone();
                let ssid = ap.ssid.clone();
                let was_saved = ap.saved;
                let h = h.clone();
                move || {
                    let psk = entry.text().to_string();
                    if psk.len() < 8 {
                        h.errors.borrow_mut().insert(
                            ssid.clone(),
                            "Min 8 characters".into(),
                        );
                        refresh_list(&h);
                        return;
                    }
                    h.errors.borrow_mut().remove(&ssid);
                    let had_path = h.pending_secret_paths.borrow().contains_key(&ssid);
                    let cmd = match h.pending_secret_paths.borrow_mut().remove(&ssid) {
                        Some(path) => BackendCmd::ProvideSecret { path, psk },
                        None => BackendCmd::ConnectSecure { ssid: ssid.clone(), psk },
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
            if h.focus_ssid.borrow().as_deref() == Some(ap.ssid.as_str()) {
                entry.grab_focus();
            }
            h.pw_entries.borrow_mut().insert(ap.ssid.clone(), entry);
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
        revealer.set_transition_duration(200);
        revealer.set_child(Some(&details));
        revealer.set_reveal_child(expanded);
        h.revealers.borrow_mut().insert(ap.ssid.clone(), revealer.clone());
        vbox.append(&revealer);

    let row = gtk4::ListBoxRow::new();
    row.set_child(Some(&vbox));

    row.set_activatable(false);
    row.add_css_class("rnet-row");
    row.add_css_class("rnet-row-hover");
    row
}

fn detail_line(text: &str) -> gtk4::Label {
    let l = gtk4::Label::new(Some(text));
    l.set_halign(gtk4::Align::Start);
    l.add_css_class("dim-label");
    l
}

fn section_label(text: &str) -> gtk4::ListBoxRow {
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

fn flash_error(h: &UiHandles, ssid: &str, message: &str) {
    const SHOW_FOR: std::time::Duration = std::time::Duration::from_secs(12);
    let deadline = std::time::Instant::now() + SHOW_FOR;
    h.errors.borrow_mut().insert(ssid.to_string(), message.to_string());
    h.err_token.borrow_mut().insert(ssid.to_string(), deadline);
    refresh_list(h);
    let h2 = h.clone();
    let ssid = ssid.to_string();
    gtk4::glib::timeout_add_local_once(SHOW_FOR, move || {
        let current = h2.err_token.borrow().get(&ssid).cloned();
        if current == Some(deadline) {
            h2.err_token.borrow_mut().remove(&ssid);
            h2.errors.borrow_mut().remove(&ssid);
            refresh_list(&h2);
        }
    });
}

fn mark_connecting(h: &UiHandles, ssid: &str, with_password: bool, unsaved: bool) {
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
    h.connecting.borrow_mut().insert(
        ssid.to_string(),
        std::time::Instant::now() + TIMEOUT,
    );
    if with_password {
        h.pw_attempt.borrow_mut().insert(ssid.to_string());
    }
    if unsaved {
        h.unsaved_attempt.borrow_mut().insert(ssid.to_string());
    }
    h.errors.borrow_mut().remove(ssid);
    let h2 = h.clone();
    let ssid = ssid.to_string();
    gtk4::glib::timeout_add_local_once(TIMEOUT, move || {
        let expired = h2
            .connecting
            .borrow()
            .get(&ssid)
            .map(|d| std::time::Instant::now() >= *d)
            .unwrap_or(false);
        let still_not_active = h2.model.borrow().active_ssid.as_deref() != Some(ssid.as_str());
        if expired && still_not_active {
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
                h2.errors.borrow_mut().insert(
                    ssid.clone(),
                    "Connection failed".into(),
                );
                refresh_list(&h2);
            }
        }
    });
}

fn hotspot_section_row(h: &UiHandles) -> gtk4::ListBoxRow {
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    vbox.set_margin_start(8);
    vbox.set_margin_end(8);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);

    if let Some(e) = h.hotspot_error.borrow().clone() {
        let el = gtk4::Label::new(Some(&e));
        el.set_halign(gtk4::Align::Start);
        el.add_css_class("error");
        el.set_wrap(true);
        vbox.append(&el);
    }
    let saved = h.model.borrow().hotspot.clone();
    let is_active = saved.as_ref().map(|x| x.active).unwrap_or(false);

    if is_active {
        let Some(hs) = saved.clone() else {
            let row = gtk4::ListBoxRow::new();
            row.set_activatable(false);
            return row;
        };
        let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let icon = gtk4::Image::from_icon_name(&themed_icon(&[
            "network-wireless-hotspot-symbolic",
            "network-wireless-symbolic",
        ]));
        icon.set_pixel_size(24);
        hbox.append(&icon);
        let name = gtk4::Label::new(Some(&hs.ssid));
        name.set_halign(gtk4::Align::Start);
        name.set_hexpand(true);
        hbox.append(&name);
        let edit = gtk4::Button::with_label("Edit");
        edit.add_css_class("flat");
        edit.set_tooltip_text(Some("Edit"));
        edit.connect_clicked(move |_| open_editor());
        hbox.append(&edit);
        hbox.append(&action_button("Stop", true, BackendCmd::StopHotspot, h));
        vbox.append(&hbox);
        if let Some(psk) = hs.psk.as_deref() {
            let key = gtk4::Label::new(Some(&format!("Password: {psk}")));
            key.set_halign(gtk4::Align::Start);
            key.add_css_class("dim-label");
            key.add_css_class("rnet-hotspot-key");
            vbox.append(&key);
        }
    } else {

        {
            let mut draft = h.hotspot_ssid.borrow_mut();
            if draft.is_empty() {
                *draft = "RnetHotspot".to_string();
            }
        }
        let ssid_entry = gtk4::Entry::new();
        ssid_entry.set_placeholder_text(Some("Name"));
        ssid_entry.set_text(&h.hotspot_ssid.borrow().clone());
        vbox.append(&ssid_entry);

        let pass_entry = gtk4::PasswordEntry::new();
        pass_entry.set_width_chars(12);
        pass_entry.set_show_peek_icon(true);
        pass_entry.set_placeholder_text(Some("Password"));
        pass_entry.set_text(&h.hotspot_psk.borrow().clone());
        vbox.append(&pass_entry);

        let btn_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        btn_row.set_halign(gtk4::Align::End);
        let cancel = gtk4::Button::with_label("Close");
        cancel.add_css_class("flat");
        let start = gtk4::Button::with_label("Start");
        start.add_css_class("suggested-action");

        let valid_now = !ssid_entry.text().trim().is_empty() && pass_entry.text().len() >= 8;
        start.set_sensitive(valid_now);
        btn_row.append(&cancel);
        btn_row.append(&start);
        vbox.append(&btn_row);
        {
            let h = h.clone();
            cancel.connect_clicked(move |_| {
                h.hotspot_expanded.set(false);
                h.hotspot_error.borrow_mut().take();
                refresh_list(&h);
            });
        }
        {
            let start_g = start.clone();
            let pass_g = pass_entry.clone();
            let draft_ssid = h.hotspot_ssid.clone();
            let draft_psk = h.hotspot_psk.clone();
            ssid_entry.connect_changed(move |e| {
                *draft_ssid.borrow_mut() = e.text().to_string();
                *draft_psk.borrow_mut() = pass_g.text().to_string();
                start_g.set_sensitive(
                    !e.text().trim().is_empty() && pass_g.text().len() >= 8,
                );
            });
        }
        {
            let start = start.clone();
            let ssid_entry = ssid_entry.clone();
            let pass_entry = pass_entry.clone();
            let draft_psk = h.hotspot_psk.clone();
            pass_entry.connect_changed(move |e| {
                *draft_psk.borrow_mut() = e.text().to_string();
                start.set_sensitive(
                    !ssid_entry.text().trim().is_empty() && e.text().len() >= 8,
                );
            });
        }
        {
            let h = h.clone();
            let ssid_entry = ssid_entry.clone();
            let pass_entry = pass_entry.clone();
            start.connect_clicked(move |_| {
                let ssid = ssid_entry.text().trim().to_string();
                let psk = pass_entry.text().to_string();
                if ssid.is_empty() {
                    *h.hotspot_error.borrow_mut() =
                        Some("Enter a name".into());
                    refresh_list(&h);
                    return;
                }
                if psk.len() < 8 {
                    *h.hotspot_error.borrow_mut() =
                        Some("Min 8 characters".into());
                    refresh_list(&h);
                    return;
                }
                h.hotspot_error.borrow_mut().take();
                h.hotspot_expanded.set(false);
                *h.hotspot_ssid.borrow_mut() = ssid.clone();
                *h.hotspot_psk.borrow_mut() = psk.clone();
                let _ = h.cmd_tx.try_send(BackendCmd::CreateHotspot { ssid, psk });
            });
        }
    }

    let row = gtk4::ListBoxRow::new();
    row.set_child(Some(&vbox));
    row.set_activatable(false);
    row
}

fn refresh_list(h: &UiHandles) {

    let adj = h.scroll.vadjustment();
    let saved_pos = adj.value();
    while let Some(child) = h.list.first_child() {
        h.list.remove(&child);
    }
    let m = h.model.borrow();
    let q = h.query.borrow().clone();
    let exp = h.expanded.borrow().clone();
    let errs = h.errors.borrow();
    let active = if m.hotspot.as_ref().map(|h| h.active).unwrap_or(false) {
        None
    } else {
        m.active_ssid.as_deref()
    };
    let aps = filter_aps(&m.sorted_aps(), &q);
    let se = expander(h);

    if h.hidden_expanded.get() || h.hidden_closing.get() {
        let (rev, row) = ensure_hidden_card(h);
        if h.hidden_closing.get() && !h.hidden_expanded.get() {
            rev.set_reveal_child(false);
        }
        h.list.append(&row);
    }

    if !m.nm_online {
        let banner = gtk4::Label::new(Some("NetworkManager unavailable"));
        banner.set_halign(gtk4::Align::Start);
        banner.add_css_class("error");
        banner.set_wrap(true);
        banner.set_margin_start(8);
        banner.set_margin_end(8);
        banner.set_margin_top(8);
        h.list.append(&banner);
    }

    let hs_active = m.hotspot.as_ref().map(|x| x.active).unwrap_or(false);
    let hs_shown = h.hotspot_expanded.get() || hs_active || h.hotspot_error.borrow().is_some();
    let hs_first = hs_shown && !h.hotspot_was.get();
    h.hotspot_was.set(hs_shown);
    if hs_shown {
        if hs_first {
            h.list.append(&animated_card(vec![
                section_label("Hotspot").upcast(),
                hotspot_section_row(h).upcast(),
            ]));
        } else {
            h.list.append(&section_label("Hotspot"));
            h.list.append(&hotspot_section_row(h));
        }
    }

    let mut connected_shown = false;
    if let Some(ssid) = active {
        let owned;
        let ap = match aps.iter().find(|ap| ap.ssid == ssid) {
            Some(ap) => ap,
            None => {
                owned = Ap {
                    ssid: ssid.to_string(),
                    bssid_path: String::from("/"),
                    strength: 0,
                    secured: true,
                    saved: true,
                };
                &owned
            }
        };
        h.list.append(&section_label("Connected"));
        let is_exp = exp.as_deref() == Some(ssid);
        h.list.append(&ap_row(
            ap,
            true,
            m.active_iface.as_deref(),
            m.active_ipv4.as_deref(),
            is_exp,
            None,
            h,
            se.clone(),
        ));
        connected_shown = true;
    }
    let available: Vec<&Ap> = aps
        .iter()
        .filter(|ap| Some(ap.ssid.as_str()) != active)
        .collect();
    if !available.is_empty() || !connected_shown {
        h.list.append(&section_label("Available"));
    }
    if available.is_empty() {
        let l = gtk4::Label::new(Some("No networks found"));
        l.add_css_class("dim-label");
        l.set_margin_top(16);
        h.list.append(&l);
    }
    for ap in available {
        let is_exp = exp.as_deref() == Some(ap.ssid.as_str());
        let err = errs.get(&ap.ssid).cloned();
        h.list.append(&ap_row(
            ap, false, None, None, is_exp, err, h, se.clone(),
        ));
    }

    let adj2 = adj.clone();
    gtk4::glib::idle_add_local_once(move || {
        let max = (adj2.upper() - adj2.page_size()).max(adj2.lower());
        adj2.set_value(saved_pos.clamp(adj2.lower(), max));
    });
}

fn hidden_form_row(h: &UiHandles) -> gtk4::ListBoxRow {
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    vbox.set_margin_start(40);
    vbox.set_margin_end(12);
    vbox.set_margin_bottom(10);

    if let Some(e) = h.hidden_error.borrow().clone() {
        let el = gtk4::Label::new(Some(&e));
        el.set_halign(gtk4::Align::Start);
        el.add_css_class("error");
        vbox.append(&el);
    }

    let ssid_entry = gtk4::Entry::new();
    ssid_entry.set_placeholder_text(Some("Hidden SSID"));
    vbox.append(&ssid_entry);

    let pass_entry = gtk4::PasswordEntry::new();
        pass_entry.set_width_chars(12);
    pass_entry.set_show_peek_icon(true);
    pass_entry.set_placeholder_text(Some("Password (optional)"));
    vbox.append(&pass_entry);

    let btn_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    btn_row.set_halign(gtk4::Align::End);
    let cancel = gtk4::Button::with_label("Cancel");
    cancel.add_css_class("flat");
    let connect = gtk4::Button::with_label("Connect");
    connect.add_css_class("suggested-action");
    btn_row.append(&cancel);
    btn_row.append(&connect);
    vbox.append(&btn_row);

    {
        let h = h.clone();
        cancel.connect_clicked(move |_| {
            collapse_hidden(&h);
        });
    }
    {
        let h = h.clone();
        let pass_entry_for_submit = pass_entry.clone();
        let submit = Rc::new(move || {
            let ssid = ssid_entry.text().trim().to_string();
            if ssid.is_empty() {
                *h.hidden_error.borrow_mut() = Some("Enter the network name".into());
                refresh_list(&h);
                return;
            }
            let psk = pass_entry_for_submit.text().to_string();

            if !psk.is_empty() && psk.len() < 8 {
                *h.hidden_error.borrow_mut() =
                    Some("Password needs at least 8 characters".into());
                refresh_list(&h);
                return;
            }
            h.hidden_error.borrow_mut().take();
            collapse_hidden(&h);
            mark_connecting(&h, &ssid, !psk.is_empty(), false);
            let _ = h.cmd_tx.try_send(BackendCmd::ConnectHidden { ssid, psk });
        });
        connect.connect_clicked({
            let submit = submit.clone();
            move |_| submit()
        });
        pass_entry.connect_activate(move |_| submit());
    }

    let row = gtk4::ListBoxRow::new();
    row.set_child(Some(&vbox));
    row.set_activatable(false);
    row
}

fn ensure_hidden_card(h: &UiHandles) -> (gtk4::Revealer, gtk4::ListBoxRow) {
    if let Some(c) = h.hidden_card.borrow().as_ref() {
        return c.clone();
    }
    let holder = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    holder.append(&section_label("Hidden network"));
    holder.append(&hidden_form_row(h));
    let rev = gtk4::Revealer::new();
    rev.set_transition_type(gtk4::RevealerTransitionType::SlideDown);
    rev.set_transition_duration(160);
    rev.set_reveal_child(false);
    rev.set_child(Some(&holder));
    let wrap = gtk4::ListBoxRow::new();
    wrap.set_activatable(false);
    wrap.set_selectable(false);
    wrap.set_child(Some(&rev));
    *h.hidden_card.borrow_mut() = Some((rev.clone(), wrap.clone()));
    (rev, wrap)
}

fn expand_hidden(h: &UiHandles) {
    h.hidden_closing.set(false);
    h.hidden_expanded.set(true);
    h.hidden_error.borrow_mut().take();
    let (rev, _) = ensure_hidden_card(h);
    let rev2 = rev.clone();
    gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(8), move || {
        rev2.set_reveal_child(true);
    });
    refresh_list(h);
}

fn collapse_hidden(h: &UiHandles) {
    h.hidden_expanded.set(false);
    h.hidden_error.borrow_mut().take();
    if h.hidden_card.borrow().is_some() {
        h.hidden_closing.set(true);
        if let Some((rev, _)) = h.hidden_card.borrow().as_ref() {
            rev.set_reveal_child(false);
        }
        let h2 = h.clone();
        let closing = h.hidden_closing.clone();
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(160), move || {
            closing.set(false);
            refresh_list(&h2);
        });
    } else {
        refresh_list(h);
    }
}

pub fn run(
    initial: Model,
    ui_rx: async_channel::Receiver<UiEvent>,
    toggle_rx: async_channel::Receiver<Option<(i32, i32)>>,
    quit_rx: async_channel::Receiver<()>,
    cmd_tx: async_channel::Sender<BackendCmd>,
) {
    gtk4::glib::log_set_default_handler(|_d, level, msg| {
        if matches!(level, gtk4::glib::LogLevel::Error) {
            eprintln!("{msg}");
        }
    });

    let app = gtk4::Application::new(Some("dev.abhinash-pdl.rnetapplet"), Default::default());

    app.connect_activate(move |app| {
        load_css();
        let ui_rx = ui_rx.clone();
        let toggle_rx = toggle_rx.clone();
        let quit_rx = quit_rx.clone();
        let cmd_tx = cmd_tx.clone();
        let model = Rc::new(RefCell::new(initial.clone()));
        let query = Rc::new(RefCell::new(String::new()));
        let visible = Rc::new(Cell::new(false));

        let prog_guard = Rc::new(Cell::new(false));
        let expanded: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let errors: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));
        let err_token: Rc<RefCell<HashMap<String, std::time::Instant>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let pw_drafts: Rc<RefCell<HashMap<String, String>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let focus_ssid: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let pending_secret_paths: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));
        let connecting: Rc<RefCell<HashMap<String, std::time::Instant>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let pw_attempt: Rc<RefCell<std::collections::HashSet<String>>> =
            Rc::new(RefCell::new(std::collections::HashSet::new()));
        let unsaved_attempt: Rc<RefCell<std::collections::HashSet<String>>> =
            Rc::new(RefCell::new(std::collections::HashSet::new()));
        let hidden_expanded: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let hidden_closing: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let hidden_card: Rc<RefCell<Option<(gtk4::Revealer, gtk4::ListBoxRow)>>> = Rc::new(RefCell::new(None));
        let hidden_error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let hotspot_error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let hotspot_expanded: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let hotspot_was: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let connect_btns: Rc<RefCell<HashMap<String, gtk4::Button>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let ssid_labels: Rc<RefCell<HashMap<String, gtk4::Label>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let hotspot_ssid: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let hotspot_psk: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let speeds: Rc<RefCell<(Option<u64>, Option<u64>)>> = Rc::new(RefCell::new((None, None)));
        let speed_labels: Rc<RefCell<Option<(gtk4::Label, gtk4::Label)>>> =
            Rc::new(RefCell::new(None));
        let revealers: Rc<RefCell<HashMap<String, gtk4::Revealer>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let chevrons: Rc<RefCell<HashMap<String, gtk4::Image>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let pw_entries: Rc<RefCell<HashMap<String, gtk4::PasswordEntry>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let hide_gen: Rc<Cell<u64>> = Rc::new(Cell::new(0));

        let window = gtk4::ApplicationWindow::new(app);
        window.set_title(Some("Networks"));
        window.add_css_class("rnet-window");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);

        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Right, true);
        window.set_anchor(Edge::Bottom, true);
        window.set_anchor(Edge::Left, true);

        window.set_exclusive_zone(0);
        window.set_keyboard_mode(KeyboardMode::OnDemand);

        {
            let visible = visible.clone();
            let hide_window = window.clone();
            window.connect_close_request(move |_| {
                visible.set(false);
                hide_window.set_visible(false);
                gtk4::glib::Propagation::Stop
            });
        }

        let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        vbox.add_css_class("rnet-card");
        vbox.set_margin_top(0);
        vbox.set_margin_bottom(0);
        vbox.set_margin_start(0);
        vbox.set_margin_end(0);
        vbox.set_size_request(200, 560);

        let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        header.add_css_class("rnet-header");
        header.set_margin_start(8);
        header.set_margin_end(8);
        let title = gtk4::Label::new(Some("Networks"));
        title.set_halign(gtk4::Align::Start);
        title.set_hexpand(true);
        title.add_css_class("rnet-title");
        header.append(&title);
        let hidden = flat_icon_button("list-add-symbolic", 24, "Hidden network");
        header.append(&hidden);
        let settings = flat_icon_button("preferences-system-symbolic", 24, "Network settings");
        header.append(&settings);
        vbox.append(&header);

        settings.connect_clicked(move |_| open_editor());

        let toolbar = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        toolbar.add_css_class("rnet-toolbar");
        toolbar.set_margin_start(4);
        toolbar.set_margin_end(4);

        let wifi_switch = gtk4::Switch::new();
        wifi_switch.set_valign(gtk4::Align::Center);
        wifi_switch.set_active(model.borrow().wifi_enabled);
        wifi_switch.set_tooltip_text(Some("Wi-Fi"));
        toolbar.append(&wifi_switch);

        let air_switch = gtk4::Switch::new();
        air_switch.set_valign(gtk4::Align::Center);
        air_switch.set_active(model.borrow().airplane_mode());
        air_switch.set_tooltip_text(Some("Airplane mode"));
        toolbar.append(&air_switch);

        {
            let tx = cmd_tx.clone();
            let prog_guard = prog_guard.clone();
            wifi_switch.connect_state_set(move |_, state| {
                if prog_guard.get() {
                    return gtk4::glib::Propagation::Proceed;
                }
                let _ = tx.try_send(BackendCmd::SetWifi(state));
                gtk4::glib::Propagation::Proceed
            });
        }
        {
            let tx = cmd_tx.clone();
            let prog_guard = prog_guard.clone();
            air_switch.connect_state_set(move |_, state| {
                if prog_guard.get() {
                    return gtk4::glib::Propagation::Proceed;
                }
                let _ = tx.try_send(BackendCmd::SetAirplane(state));
                gtk4::glib::Propagation::Proceed
            });
        }

        let hotspot_icon = gtk4::Image::from_icon_name(&themed_icon(&["network-wireless-hotspot-symbolic", "network-wireless-hotspot", "hotspot-symbolic", "network-wireless-symbolic"]));
        hotspot_icon.set_pixel_size(18);
        let hotspot_btn = gtk4::Button::new();
        hotspot_btn.set_child(Some(&hotspot_icon));
        hotspot_btn.set_focus_on_click(false);
        hotspot_btn.set_focusable(false);
        hotspot_btn.set_tooltip_text(Some("Hotspot"));
        toolbar.append(&hotspot_btn);

        let search = gtk4::SearchEntry::new();
        search.set_placeholder_text(Some("Search..."));
        search.set_width_chars(2);
        search.set_hexpand(true);
        toolbar.append(&search);

        let qr_icon =
            gtk4::Image::from_icon_name(&themed_icon(&["scanner-symbolic", "camera-photo-symbolic"]));
        qr_icon.set_pixel_size(22);
        let qr = gtk4::Button::new();
        qr.set_child(Some(&qr_icon));
        qr.set_tooltip_text(Some("Scan QR"));
        qr.add_css_class("rnet-qr");
        qr.set_focusable(false);
        qr.set_focus_on_click(false);
        toolbar.append(&qr);

        vbox.append(&toolbar);

        let scroll = gtk4::ScrolledWindow::new();
        scroll.set_vexpand(true);
        scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        let list = gtk4::ListBox::new();
        list.set_selection_mode(gtk4::SelectionMode::None);
        scroll.set_child(Some(&list));
        vbox.append(&scroll);

        let backdrop = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        backdrop.set_hexpand(true);
        backdrop.set_vexpand(true);
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&backdrop));
        window.set_child(Some(&overlay));

        let popup_revealer = gtk4::Revealer::new();
        popup_revealer.add_css_class("rnet-popup");
        popup_revealer.set_transition_type(gtk4::RevealerTransitionType::SlideDown);
        popup_revealer.set_transition_duration(130);
        popup_revealer.set_child(Some(&vbox));
        popup_revealer.set_halign(gtk4::Align::End);
        popup_revealer.set_valign(gtk4::Align::Start);
        popup_revealer.set_size_request(200, -1);
        overlay.add_overlay(&popup_revealer);

        {
            let window_click = window.clone();
            let revealer_click = popup_revealer.clone();
            let visible_click = visible.clone();
            let hide_gen_click = hide_gen.clone();
            let gesture = gtk4::GestureClick::new();
            gesture.connect_pressed(move |_, _, _, _| {
                if visible_click.get() {
                    hide_popup(
                        &window_click,
                        &revealer_click,
                        &visible_click,
                        &hide_gen_click,
                    );
                }
            });
            backdrop.add_controller(gesture);
        }

        let h = UiHandles {
            list: list.clone(),
            scroll: scroll.clone(),
            model: model.clone(),
            query: query.clone(),
            expanded: expanded.clone(),
            errors: errors.clone(),
            err_token: err_token.clone(),
            pw_drafts: pw_drafts.clone(),
            focus_ssid: focus_ssid.clone(),
            pending_secret_paths: pending_secret_paths.clone(),
            connecting: connecting.clone(),
            pw_attempt: pw_attempt.clone(),
            unsaved_attempt: unsaved_attempt.clone(),
            hidden_expanded: hidden_expanded.clone(),
            hidden_closing: hidden_closing.clone(),
            hidden_card: hidden_card.clone(),
            hidden_error: hidden_error.clone(),
            hotspot_error: hotspot_error.clone(),
            hotspot_expanded: hotspot_expanded.clone(),
            hotspot_was: hotspot_was.clone(),
            hotspot_ssid: hotspot_ssid.clone(),
            hotspot_psk: hotspot_psk.clone(),
            speeds: speeds.clone(),
            speed_labels: speed_labels.clone(),
            revealers: revealers.clone(),
            chevrons: chevrons.clone(),
            connect_btns: connect_btns.clone(),
            ssid_labels: ssid_labels.clone(),
            pw_entries: pw_entries.clone(),
            cmd_tx: cmd_tx.clone(),
        };

        {
            let h = h.clone();
            hidden.connect_clicked(move |_| {
                if h.hidden_expanded.get() {
                    collapse_hidden(&h);
                } else {
                    expand_hidden(&h);
                }
            });
        }

        {
            let h = h.clone();
            hotspot_btn.connect_clicked(move |_| {
                let hs = h.model.borrow().hotspot.clone();
                if hs.as_ref().map(|x| x.active).unwrap_or(false) {
                    h.hotspot_error.borrow_mut().take();
                    let _ = h.cmd_tx.try_send(BackendCmd::StopHotspot);
                    return;
                }

                if let Some(cfg) = hs {
                    let psk = h.hotspot_psk.borrow().clone();
                    let psk = if psk.is_empty() {
                        cfg.psk.clone().unwrap_or_default()
                    } else {
                        psk
                    };
                    let ssid = if h.hotspot_ssid.borrow().is_empty() {
                        cfg.ssid.clone()
                    } else {
                        h.hotspot_ssid.borrow().clone()
                    };
                    if !ssid.trim().is_empty() && psk.len() >= 8 {
                        h.hotspot_error.borrow_mut().take();
                        let _ = h.cmd_tx.try_send(BackendCmd::CreateHotspot { ssid, psk });
                        return;
                    }
                }

                h.hotspot_expanded.set(!h.hotspot_expanded.get());
                if !h.hotspot_expanded.get() {
                    h.hotspot_error.borrow_mut().take();
                }
                refresh_list(&h);
            });
        }

        {
            let tx = cmd_tx.clone();
            if let Some(settings) = gtk4::Settings::default() {
                let tx2 = tx.clone();
                settings.connect_gtk_icon_theme_name_notify(move |_| {
                    let _ = tx.try_send(BackendCmd::RefreshTray);
                });
                settings.connect_gtk_theme_name_notify(move |_| {
                    let _ = tx2.try_send(BackendCmd::RefreshTray);
                });
            }
        }

        {
            let app = app.clone();
            let cmd_tx = cmd_tx.clone();
            qr.connect_clicked(move |_| {
                crate::qr_scan::open_scanner(&app, cmd_tx.clone());
            });
        }

        refresh_list(&h);
        window.set_visible(false);

        {
            let h = h.clone();
            search.connect_search_changed(move |entry| {
                *h.query.borrow_mut() = entry.text().to_string();
                refresh_list(&h);
            });
        }

        {
            let window_esc = window.clone();
            let revealer_esc = popup_revealer.clone();
            let visible = visible.clone();
            let hide_gen = hide_gen.clone();
            let keys = gtk4::EventControllerKey::new();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == Key::Escape {
                    hide_popup(&window_esc, &revealer_esc, &visible, &hide_gen);
                    gtk4::glib::Propagation::Stop
                } else {
                    gtk4::glib::Propagation::Proceed
                }
            });
            window.add_controller(keys);
        }

        {
            let h = h.clone();
            let wifi_switch = wifi_switch.clone();
            let air_switch = air_switch.clone();
            let prog_guard = prog_guard.clone();
            let hotspot_btn = hotspot_btn.clone();
            let ctx = gtk4::glib::MainContext::default();
            ctx.spawn_local(async move {
                while let Ok(ev) = ui_rx.recv().await {
                    match ev {
                        UiEvent::Model(m) => {
                            if wifi_switch.state() != m.wifi_enabled && !m.airplane_mode() {
                                prog_guard.set(true);
                                wifi_switch.set_active(m.wifi_enabled);
                                prog_guard.set(false);
                            }
                            if air_switch.state() != m.airplane_mode() {
                                prog_guard.set(true);
                                air_switch.set_active(m.airplane_mode());
                                prog_guard.set(false);
                            }
                            wifi_switch.set_sensitive(!m.airplane_mode());
                            if m.airplane_mode() && wifi_switch.state() {
                                prog_guard.set(true);
                                wifi_switch.set_active(false);
                                prog_guard.set(false);
                            }
                            let hs_active = m.hotspot.as_ref().map(|x| x.active).unwrap_or(false);
                            if hs_active {
                                hotspot_btn.add_css_class("suggested-action");
                            } else {
                                hotspot_btn.remove_css_class("suggested-action");
                            }

                            if let Some(active) = m.active_ssid.as_deref() {
                                h.focus_ssid.borrow_mut().take();
                                h.errors.borrow_mut().remove(active);

                                h.connecting.borrow_mut().clear();
                                h.pw_attempt.borrow_mut().remove(active);
                                h.unsaved_attempt.borrow_mut().remove(active);
                                h.pw_drafts.borrow_mut().remove(active);
                                h.err_token.borrow_mut().remove(active);
                            }

                            {
                                let was_active = h
                                    .model
                                    .borrow()
                                    .hotspot
                                    .as_ref()
                                    .map(|x| x.active)
                                    .unwrap_or(false);
                                if hs_active && !was_active {
                                    h.hotspot_expanded.set(true);
                                } else if was_active && !hs_active {
                                    h.hotspot_expanded.set(false);
                                    h.hotspot_error.borrow_mut().take();
                                }
                            }

                            if let Some(hs) = m.hotspot.as_ref() {
                                if h.hotspot_ssid.borrow().is_empty() {
                                    *h.hotspot_ssid.borrow_mut() = hs.ssid.clone();
                                }
                                if h.hotspot_psk.borrow().is_empty() {
                                    if let Some(psk) = hs.psk.clone() {
                                        *h.hotspot_psk.borrow_mut() = psk;
                                    }
                                }
                            }
                            *h.model.borrow_mut() = m;
                            refresh_list(&h);
                        }
                        UiEvent::SecretsNeeded { ssid, path, request_new } => {
                            h.connecting.borrow_mut().remove(&ssid);
                            if request_new {
                                if h.unsaved_attempt.borrow_mut().remove(&ssid) {
                                    h.pending_secret_paths.borrow_mut().remove(&ssid);
                                    let _ = h.cmd_tx.try_send(BackendCmd::Forget(ssid.clone()));
                                } else {
                                    h.pending_secret_paths.borrow_mut().insert(ssid.clone(), path);
                                }
                                h.pw_drafts.borrow_mut().remove(&ssid);
                                *h.expanded.borrow_mut() = Some(ssid.clone());
                                *h.focus_ssid.borrow_mut() = Some(ssid.clone());
                                flash_error(&h, &ssid, "Incorrect password — try again");
                            } else {
                                h.errors.borrow_mut().remove(&ssid);
                                h.err_token.borrow_mut().remove(&ssid);
                                h.pending_secret_paths.borrow_mut().insert(ssid.clone(), path);
                                *h.expanded.borrow_mut() = Some(ssid.clone());
                                *h.focus_ssid.borrow_mut() = Some(ssid);
                                refresh_list(&h);
                            }
                        }
                        UiEvent::SsidError { ssid, message } => {
                            h.connecting.borrow_mut().remove(&ssid);
                            h.pw_attempt.borrow_mut().remove(&ssid);
                            h.unsaved_attempt.borrow_mut().remove(&ssid);
                            *h.expanded.borrow_mut() = Some(ssid.clone());
                            *h.focus_ssid.borrow_mut() = Some(ssid.clone());
                            flash_error(&h, &ssid, &message);
                        }
                        UiEvent::BackendError(message) => {
                            *h.hotspot_error.borrow_mut() = Some(message);
                            refresh_list(&h);
                        }
                        UiEvent::Speeds { up_bps, down_bps } => {
                            *h.speeds.borrow_mut() = (up_bps, down_bps);
                            if let Some((down_label, up_label)) = h.speed_labels.borrow().clone() {
                                down_label.set_text(&format!(
                                    "↓ {}",
                                    crate::state::format_rate(down_bps)
                                ));
                                up_label.set_text(&format!(
                                    "↑ {}",
                                    crate::state::format_rate(up_bps)
                                ));
                            }
                        }
                    }
                }
            });
        }

        {
            let window = window.clone();
            let visible = visible.clone();
            let hide_gen = hide_gen.clone();
            let h_open = h.clone();
            let search_open = search.clone();
            let ctx = gtk4::glib::MainContext::default();
            ctx.spawn_local(async move {
                while let Ok(pos) = toggle_rx.recv().await {
                    tracing::debug!("gtk toggling popup");
                    if visible.get() {
                        hide_popup(&window, &popup_revealer, &visible, &hide_gen);
                        continue;
                    }
                    if let Some((x, y)) = pos {
                        place_near(&popup_revealer, x, y);
                    }
                    *h_open.expanded.borrow_mut() = None;
                    h_open.focus_ssid.borrow_mut().take();
                    h_open.connecting.borrow_mut().clear();
                    h_open.errors.borrow_mut().clear();
                    h_open.err_token.borrow_mut().clear();
                    h_open.pw_drafts.borrow_mut().clear();
                    h_open.pending_secret_paths.borrow_mut().clear();
                    h_open.pw_attempt.borrow_mut().clear();
                    h_open.unsaved_attempt.borrow_mut().clear();
                    h_open.hidden_expanded.set(false);
                    h_open.hidden_closing.set(false);
                    h_open.hidden_error.borrow_mut().take();
                    h_open.hotspot_error.borrow_mut().take();
                    h_open.hotspot_expanded.set(false);
                    *h_open.speeds.borrow_mut() = (None, None);
                    *h_open.speed_labels.borrow_mut() = None;
                    *h_open.query.borrow_mut() = String::new();
                    search_open.set_text("");
                    refresh_list(&h_open);
                    visible.set(true);
                    window.set_visible(true);
                    window.present();
                    hide_gen.set(hide_gen.get() + 1);
                    popup_revealer.set_reveal_child(false);
                    {
                        let v = visible.clone();
                        let pop_reveal = popup_revealer.clone();
                        gtk4::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(8),
                            move || {
                                if v.get() {
                                    pop_reveal.add_css_class("open");
                                    pop_reveal.set_reveal_child(true);
                                }
                            },
                        );
                    }
                }
            });
        }

        {
            let app = app.clone();
            let ctx = gtk4::glib::MainContext::default();
            ctx.spawn_local(async move {
                if quit_rx.recv().await.is_ok() {
                    app.quit();
                }
            });
        }
    });

    app.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ap(ssid: &str, strength: u8) -> Ap {
        Ap {
            ssid: ssid.into(),
            bssid_path: "/".into(),
            strength,
            secured: true,
            saved: false,
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
    fn signal_icon_steps() {
        assert_eq!(signal_icon_name(0), "network-wireless-signal-none-symbolic");
        assert_eq!(signal_icon_name(20), "network-wireless-signal-weak-symbolic");
        assert_eq!(signal_icon_name(50), "network-wireless-signal-ok-symbolic");
        assert_eq!(signal_icon_name(70), "network-wireless-signal-good-symbolic");
        assert_eq!(signal_icon_name(95), "network-wireless-signal-excellent-symbolic");
    }
}
