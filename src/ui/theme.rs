use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk4::cairo;
use gtk4::prelude::*;

pub(crate) type StrengthSetter = Rc<dyn Fn(u8)>;

const STEPS: usize = 5;

thread_local! {
    static THEMED_CACHE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static LOOKUP_CACHE: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());

    static SIGNAL_NAMES: RefCell<[Option<String>; STEPS]> = RefCell::new(Default::default());
    static PAPIRUS: Cell<Option<bool>> = const { Cell::new(None) };
    static ADWAITA: Cell<Option<bool>> = const { Cell::new(None) };
}

pub(crate) fn clear_caches() {
    THEMED_CACHE.with(|c| c.borrow_mut().clear());
    LOOKUP_CACHE.with(|c| c.borrow_mut().clear());
    SIGNAL_NAMES.with(|c| *c.borrow_mut() = Default::default());
    PAPIRUS.set(None);
    ADWAITA.set(None);
}

fn theme_named(needle: &str, slot: &'static std::thread::LocalKey<Cell<Option<bool>>>) -> bool {
    if let Some(v) = slot.with(|c| c.get()) {
        return v;
    }
    let v = gtk4::Settings::default()
        .and_then(|s| s.gtk_icon_theme_name())
        .map(|n| n.as_str().to_lowercase().contains(needle))
        .unwrap_or(false);
    slot.with(|c| c.set(Some(v)));
    v
}

pub(crate) fn signal_step(strength: u8) -> &'static str {
    match strength {
        0..=5 => "none",
        6..=30 => "weak",
        31..=55 => "ok",
        56..=80 => "good",
        _ => "excellent",
    }
}

pub(crate) fn bar_count(strength: u8) -> usize {
    match strength {
        0..=5 => 0,
        6..=30 => 1,
        31..=55 => 2,
        56..=80 => 3,
        _ => 4,
    }
}

fn lookup_ok(name: &str) -> bool {
    if let Some(v) = LOOKUP_CACHE.with(|c| c.borrow().get(name).copied()) {
        return v;
    }
    let has = gtk4::gdk::Display::default()
        .map(|d| gtk4::IconTheme::for_display(&d).has_icon(name))
        .unwrap_or(false);
    LOOKUP_CACHE.with(|c| c.borrow_mut().insert(name.to_string(), has));
    has
}

pub(crate) fn themed_icon(candidates: &[&str]) -> String {
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

pub(crate) fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, PI + FRAC_PI_2);
    cr.close_path();
}

pub(crate) fn signal_bars(strength: u8) -> (gtk4::DrawingArea, StrengthSetter) {
    let level = Cell::new(strength);
    let area = gtk4::DrawingArea::new();
    area.set_content_width(24);
    area.set_content_height(24);
    area.set_valign(gtk4::Align::Center);
    let draw_level = level.clone();
    area.set_draw_func(move |w, cr, _width, _height| {
        let filled = bar_count(draw_level.get());
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
    let redraw = area.clone();
    let set: StrengthSetter = Rc::new(move |s| {
        if level.get() != s {
            level.set(s);
            redraw.queue_draw();
        }
    });
    (area, set)
}

pub(crate) fn lock_emblem() -> gtk4::Image {
    let lock = gtk4::Image::from_icon_name(&themed_icon(&[
        "lock-symbolic",
        "network-wireless-encrypted-symbolic",
        "system-lock-screen-symbolic",
    ]));
    lock.set_pixel_size(10);
    lock.add_css_class("rnet-lock-badge");
    lock.set_halign(gtk4::Align::End);
    lock.set_valign(gtk4::Align::End);
    lock.set_margin_bottom(3);
    lock.set_margin_end(0);
    lock
}

pub(crate) fn custom_lock() -> gtk4::Widget {
    let area = gtk4::DrawingArea::new();
    area.set_content_width(12);
    area.set_content_height(12);
    area.add_css_class("rnet-lock-badge");
    area.set_halign(gtk4::Align::Start);
    area.set_valign(gtk4::Align::Start);
    area.set_margin_top(20);
    area.set_margin_start(14);
    area.set_draw_func(|w, cr, _width, _height| {
        let fg = w.color();
        cr.set_source_rgba(
            fg.red() as f64,
            fg.green() as f64,
            fg.blue() as f64,
            fg.alpha() as f64,
        );
        cr.new_sub_path();
        cr.arc(
            6.0,
            6.0,
            2.8,
            std::f64::consts::PI,
            2.0 * std::f64::consts::PI,
        );
        cr.set_line_width(1.5);
        let _ = cr.stroke();
        rounded_rect(cr, 2.6, 5.6, 6.8, 5.0, 1.0);
        let _ = cr.fill();
        cr.arc(6.0, 8.2, 1.0, 0.0, 2.0 * std::f64::consts::PI);
        let _ = cr.fill();
    });
    area.upcast()
}

pub(crate) fn bars_with_lock(strength: u8) -> (gtk4::Widget, StrengthSetter) {
    let (bars, set) = signal_bars(strength);
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&bars));
    overlay.add_overlay(&lock_emblem());
    overlay.set_size_request(24, 24);
    (overlay.upcast(), set)
}

pub(crate) fn bars_with_lock_custom(strength: u8) -> (gtk4::Widget, StrengthSetter) {
    let (bars, set) = signal_bars(strength);
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&bars));
    overlay.add_overlay(&custom_lock());
    overlay.set_size_request(24, 24);
    (overlay.upcast(), set)
}

fn centered_image(name: &str) -> gtk4::Image {
    let img = gtk4::Image::from_icon_name(name);
    img.set_pixel_size(24);
    img.set_valign(gtk4::Align::Center);
    img.set_halign(gtk4::Align::Center);
    img
}

pub(crate) fn header_icon_px() -> i32 {
    if theme_named("adwaita", &ADWAITA) {
        20
    } else {
        24
    }
}

fn papirus_theme() -> bool {
    theme_named("papirus", &PAPIRUS)
}

fn step_index(strength: u8) -> usize {
    match strength {
        0..=5 => 0,
        6..=30 => 1,
        31..=55 => 2,
        56..=80 => 3,
        _ => 4,
    }
}

fn themed_signal_name(strength: u8) -> String {
    let i = step_index(strength);
    if let Some(v) = SIGNAL_NAMES.with(|c| c.borrow()[i].clone()) {
        return v;
    }
    let step = signal_step(strength);
    let sym = format!("network-wireless-signal-{step}-symbolic");
    let plain = format!("network-wireless-signal-{step}");
    let found = if lookup_ok(&sym) {
        Some(sym)
    } else if lookup_ok(&plain) {
        Some(plain)
    } else {
        None
    };
    SIGNAL_NAMES.with(|c| c.borrow_mut()[i] = found.clone());
    found.unwrap_or_default()
}

pub(crate) fn net_icon_live(strength: u8, secured: bool) -> (gtk4::Widget, StrengthSetter) {
    if papirus_theme() {
        if !secured {
            let (bars, set) = signal_bars(strength);
            return (bars.upcast(), set);
        }
        return bars_with_lock_custom(strength);
    }
    let base = themed_signal_name(strength);
    if base.is_empty() {
        if secured {
            return bars_with_lock(strength);
        }
        let (bars, set) = signal_bars(strength);
        return (bars.upcast(), set);
    }
    let icon = centered_image(&base);
    if !secured {
        let img = icon.clone();
        let set: StrengthSetter = Rc::new(move |s| {
            let name = themed_signal_name(s);
            if !name.is_empty() {
                img.set_icon_name(Some(&name));
            }
        });
        return (icon.upcast(), set);
    }
    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(&icon));
    overlay.add_overlay(&lock_emblem());
    overlay.set_size_request(24, 24);
    let img = icon.clone();
    let set: StrengthSetter = Rc::new(move |s| {
        let name = themed_signal_name(s);
        if !name.is_empty() {
            img.set_icon_name(Some(&name));
        }
    });
    (overlay.upcast(), set)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_steps() {
        assert_eq!(signal_step(0), "none");
        assert_eq!(signal_step(20), "weak");
        assert_eq!(signal_step(50), "ok");
        assert_eq!(signal_step(70), "good");
        assert_eq!(signal_step(95), "excellent");
    }

    #[test]
    fn bar_counts_are_monotonic() {
        assert_eq!(bar_count(0), 0);
        assert_eq!(bar_count(5), 0);
        assert_eq!(bar_count(6), 1);
        assert_eq!(bar_count(31), 2);
        assert_eq!(bar_count(56), 3);
        assert_eq!(bar_count(100), 4);
    }
}
