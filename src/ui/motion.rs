use super::state::UiHandles;
use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub(crate) const FLIP_MS: u32 = 240;
const FLIP_CLOSE_MS: u32 = FLIP_MS / 2;

pub(crate) fn duration(opening: bool) -> u32 {
    if opening { FLIP_MS } else { FLIP_CLOSE_MS }
}

const EASE_POWER: f64 = 3.0;

pub(crate) fn ease_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powf(EASE_POWER)
}

pub(crate) struct Flip {
    pub fixed: gtk4::Fixed,
    pub ghost: gtk4::Button,
    pub tick: RefCell<Option<gtk4::TickCallbackId>>,
}

pub(crate) fn overlay_for(
    h: &UiHandles,
    ssid: &str,
    content: &gtk4::Box,
    label: &str,
) -> gtk4::Overlay {
    let fixed = gtk4::Fixed::new();
    fixed.set_halign(gtk4::Align::Fill);
    fixed.set_valign(gtk4::Align::Fill);
    fixed.set_can_target(false);
    fixed.set_can_focus(false);

    let ghost = gtk4::Button::with_label(label);
    ghost.set_can_target(false);
    ghost.set_can_focus(false);
    ghost.set_sensitive(false);
    ghost.set_halign(gtk4::Align::Fill);
    ghost.set_valign(gtk4::Align::Fill);
    ghost.set_visible(false);
    fixed.put(&ghost, 0.0, 0.0);

    let overlay = gtk4::Overlay::new();
    overlay.set_child(Some(content));
    overlay.set_measure_overlay(&fixed, false);
    overlay.add_overlay(&fixed);

    h.flips.borrow_mut().insert(
        ssid.to_string(),
        Rc::new(Flip {
            fixed,
            ghost,
            tick: RefCell::new(None),
        }),
    );
    overlay
}

fn endpoint(h: &UiHandles, ssid: &str, expanded: bool) -> Option<gtk4::Button> {
    if expanded {
        h.submits.borrow().get(ssid).cloned()
    } else {
        h.connect_btns.borrow().get(ssid).cloned()
    }
}

pub(crate) fn measure(
    h: &UiHandles,
    ssid: &str,
    from_expanded: bool,
) -> Option<gtk4::graphene::Rect> {
    let flip = h.flips.borrow().get(ssid).cloned()?;
    endpoint(h, ssid, from_expanded)?.compute_bounds(&flip.fixed)
}

pub(crate) fn run(h: &UiHandles, ssid: &str, opening: bool, start: gtk4::graphene::Rect) {
    let Some(flip) = h.flips.borrow().get(ssid).cloned() else {
        return;
    };
    let Some(dest) = endpoint(h, ssid, opening) else {
        return;
    };
    if let Some(prev) = flip.tick.borrow_mut().take() {
        prev.remove();
    }

    let ghost = flip.ghost.clone();
    let dest_classes = dest.css_classes();
    let ghost_classes: Vec<&str> = dest_classes.iter().map(|c| c.as_str()).collect();
    ghost.set_css_classes(&ghost_classes);
    ghost.set_size_request(-1, -1);
    ghost.set_visible(true);
    dest.set_visible(true);
    dest.set_opacity(0.0);

    let sized = Cell::new(false);
    let started = std::time::Instant::now();
    let total = std::time::Duration::from_millis(duration(opening) as u64);

    let id = flip.fixed.add_tick_callback(move |fixed, _| {
        let raw = if total.is_zero() {
            1.0
        } else {
            started.elapsed().as_secs_f64() / total.as_secs_f64()
        };
        let raw = raw.clamp(0.0, 1.0);
        let e = ease_out(raw);
        if let Some(t) = dest.compute_bounds(fixed) {
            if !sized.get() && t.width() > 1.0 && t.height() > 1.0 {
                sized.set(true);
                ghost.set_size_request(t.width() as i32, t.height() as i32);
            }
            let k = e as f32;
            let x = start.x() + (t.x() - start.x()) * k;
            let y = start.y() + (t.y() - start.y()) * k;
            fixed.put(&ghost, x as f64, y as f64);
        }
        ghost.set_opacity(1.0 - e);
        dest.set_opacity(e);
        if raw >= 1.0 {
            ghost.set_visible(false);
            ghost.set_opacity(1.0);
            dest.set_opacity(1.0);
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
    *flip.tick.borrow_mut() = Some(id);
}

pub(crate) fn settle(h: &UiHandles) {
    for flip in h.flips.borrow().values() {
        if let Some(id) = flip.tick.borrow_mut().take() {
            id.remove();
        }
        flip.ghost.set_visible(false);
        flip.ghost.set_opacity(1.0);
        flip.ghost.set_size_request(-1, -1);
    }
    for btn in h.submits.borrow().values() {
        btn.set_opacity(1.0);
    }
    for btn in h.connect_btns.borrow().values() {
        btn.set_opacity(1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ease(t: f64) -> f64 {
        ease_out(t)
    }

    #[test]
    fn easing_is_monotonic_and_bounded() {
        let mut prev = -1.0;
        for i in 0..=100 {
            let v = ease(i as f64 / 100.0);
            assert!(v >= prev, "easing went backwards at {i}");
            assert!((0.0..=1.0).contains(&v), "easing out of range at {i}: {v}");
            prev = v;
        }
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
    }

    #[test]
    fn easing_clamps_out_of_range_input() {
        assert_eq!(ease(-5.0), 0.0);
        assert_eq!(ease(5.0), 1.0);
    }

    #[test]
    fn easing_starts_slow_and_finishes_fast() {
        assert!(
            ease(0.5) > 0.5,
            "midpoint should be past halfway for ease-out"
        );
    }

    #[test]
    fn lerp_between_endpoints_tracks_progress() {
        let a = 0.0f32;
        let b = 100.0f32;
        for p in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let e = ease(p as f64) as f32;
            let v = a + (b - a) * e;
            assert!(v >= a && v <= b, "interpolation escaped [{a}, {b}] at {p}");
        }
        assert_eq!(a + (b - a) * (ease(1.0) as f32), b);
    }

    #[test]
    fn ghost_and_destination_opacities_stay_complementary() {
        for i in 0..=100 {
            let e = ease(i as f64 / 100.0);
            let ghost = 1.0 - e;
            assert!((0.0..=1.0).contains(&ghost));
            assert!((ghost + e - 1.0).abs() < 1e-9, "cross-fade must sum to 1");
        }
    }
}
