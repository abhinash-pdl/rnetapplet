use gtk4::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

use super::{POPUP_MS, POPUP_W};

pub(crate) fn open_editor() {
    if let Err(e) = std::process::Command::new("nm-connection-editor").spawn() {
        tracing::warn!("could not launch nm-connection-editor: {e}");
    }
}

pub(crate) fn output_scale_for_x(x: Option<i32>) -> i32 {
    let Some(display) = gtk4::gdk::Display::default() else {
        return 1;
    };
    let monitors = display.monitors();
    let mut first = 1;
    for i in 0..monitors.n_items() {
        let Some(obj) = monitors.item(i) else {
            continue;
        };
        let Ok(m) = obj.downcast::<gtk4::gdk::Monitor>() else {
            continue;
        };
        if i == 0 {
            first = m.scale_factor().max(1);
        }
        if let Some(x) = x {
            let g = m.geometry();
            if x >= g.x() && x <= g.x() + g.width() {
                return m.scale_factor().max(1);
            }
        }
    }
    first
}

pub(crate) fn place_near(revealer: &gtk4::Revealer, x: i32) {
    let scale = output_scale_for_x(Some(x));
    let w = (POPUP_W as f64 / scale as f64).ceil() as i32;
    let mon_w = tray_monitor_width(x).unwrap_or(1920);
    let right = (mon_w - x - w / 2).clamp(2, (mon_w - w - 2).max(2));
    tracing::debug!(x, mon_w, right, w, "popup placed");
    revealer.set_margin_end(right);
    revealer.set_margin_top(0);
}

pub(crate) fn tray_monitor_width(x: i32) -> Option<i32> {
    let display = gtk4::gdk::Display::default()?;
    let monitors = display.monitors();
    let mut rightmost: Option<(i32, i32)> = None;
    for i in 0..monitors.n_items() {
        let Some(obj) = monitors.item(i) else {
            continue;
        };
        let Ok(m) = obj.downcast::<gtk4::gdk::Monitor>() else {
            continue;
        };
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
    rightmost.map(|(_, w)| w)
}

pub(crate) fn hide_popup(
    window: &gtk4::ApplicationWindow,
    revealer: &gtk4::Revealer,
    visible: &Rc<Cell<bool>>,
    hide_gen: &Rc<Cell<u64>>,
) {
    visible.set(false);
    revealer.set_reveal_child(false);
    let g = hide_gen.get() + 1;
    hide_gen.set(g);
    let seen = hide_gen.clone();
    let window = window.clone();
    let delay = std::time::Duration::from_millis(POPUP_MS as u64 + 40);
    gtk4::glib::timeout_add_local_once(delay, move || {
        if seen.get() == g {
            window.set_visible(false);
        }
    });
}
