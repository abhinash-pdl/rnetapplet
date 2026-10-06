use gtk4::prelude::*;
use std::cell::Cell;
use std::rc::Rc;

use super::POPUP_W;

unsafe extern "C" {
    fn malloc_trim(pad: usize) -> i32;
}

pub(crate) fn trim_memory() {
    unsafe { malloc_trim(0) };
}

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

pub(crate) fn place_near(revealer: &gtk4::Revealer, x: i32, y: i32, card_w: i32, card_h: i32) {
    let scale = output_scale_for_x(Some(x));
    let w = if card_w > 0 {
        card_w
    } else {
        (POPUP_W as f64 / scale as f64).ceil() as i32
    };
    let Some((mon_x, mon_y, mon_w, mon_h)) = monitor_at(x) else {
        tracing::warn!(x, "no monitor found; skipping popup placement");
        return;
    };
    let local_x = (x - mon_x).clamp(0, mon_w);
    let local_y = (y - mon_y).clamp(0, mon_h);
    let room = (mon_w - w).max(0);
    let margin_end = if local_x * 2 >= mon_w {
        (mon_w - w / 2 - local_x).clamp(0, room)
    } else {
        let m = (local_x - w / 2).clamp(0, room);
        (mon_w - m - w).max(0)
    };
    let band = (mon_h / 8).max(48);
    let bottom = local_y >= mon_h - band;
    let center = !bottom && local_y > band;
    let margin_top = if center {
        ((mon_h - card_h.max(0)) / 2).max(0)
    } else {
        0
    };
    revealer.set_halign(gtk4::Align::End);
    revealer.set_margin_end(margin_end);
    if bottom {
        revealer.set_valign(gtk4::Align::End);
        revealer.set_margin_top(0);
        revealer.set_margin_bottom(0);
    } else {
        revealer.set_valign(gtk4::Align::Start);
        revealer.set_margin_top(margin_top);
        revealer.set_margin_bottom(0);
    }
    tracing::debug!(
        x,
        y,
        mon_x,
        mon_y,
        mon_w,
        mon_h,
        w,
        card_h,
        margin_end,
        margin_top,
        bottom,
        "popup placed"
    );
}

pub(crate) fn monitor_at(x: i32) -> Option<(i32, i32, i32, i32)> {
    let display = gtk4::gdk::Display::default()?;
    let monitors = display.monitors();
    for i in 0..monitors.n_items() {
        let Some(obj) = monitors.item(i) else {
            continue;
        };
        let Ok(m) = obj.downcast::<gtk4::gdk::Monitor>() else {
            continue;
        };
        let g = m.geometry();
        if x >= g.x() && x <= g.x() + g.width() {
            return Some((g.x(), g.y(), g.width(), g.height()));
        }
    }
    let m = monitors.item(0)?.downcast::<gtk4::gdk::Monitor>().ok()?;
    let g = m.geometry();
    Some((g.x(), g.y(), g.width(), g.height()))
}

pub(crate) fn hide_popup(
    window: &gtk4::ApplicationWindow,
    outer: &gtk4::Revealer,
    size: &crate::ui::motion::SizeAnim,
    visible: &Rc<Cell<bool>>,
    hide_gen: &Rc<Cell<u64>>,
) {
    visible.set(false);
    let g = hide_gen.get() + 1;
    hide_gen.set(g);

    size.stop();
    let seen = hide_gen.clone();
    let outer_c = outer.clone();
    gtk4::glib::timeout_add_local_once(
        std::time::Duration::from_millis(crate::ui::motion::CLOSE_DELAY_MS as u64),
        move || {
            if seen.get() == g {
                outer_c.set_reveal_child(false);
            }
        },
    );
    let seen = hide_gen.clone();
    let window = window.clone();
    gtk4::glib::timeout_add_local_once(
        std::time::Duration::from_millis(
            (crate::ui::motion::CLOSE_DELAY_MS + crate::ui::motion::REVEAL_MS) as u64,
        ),
        move || {
            if seen.get() == g {
                window.set_visible(false);
                trim_memory();
            }
        },
    );
    let seen = hide_gen.clone();
    gtk4::glib::timeout_add_local_once(
        std::time::Duration::from_millis(
            (crate::ui::motion::CLOSE_DELAY_MS + crate::ui::motion::REVEAL_MS + 2500) as u64,
        ),
        move || {
            if seen.get() == g {
                trim_memory();
            }
        },
    );
}
