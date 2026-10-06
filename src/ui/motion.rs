use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gtk4::{TickCallbackId, glib::ControlFlow, prelude::*};

pub(crate) const REVEAL_MS: u32 = 220;

pub(crate) const CLOSE_DELAY_MS: u32 = 40;

pub(crate) const FIT_GAP_MS: i64 = 120;

pub(crate) const MIN_HEIGHT_JITTER: i32 = 8;

pub(crate) fn now_ms() -> i64 {
    gtk4::glib::monotonic_time() / 1000
}

pub(crate) fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub(crate) struct SizeAnim {
    apply: Rc<dyn Fn(i32)>,
    widget: gtk4::Widget,
    from: Cell<i32>,
    pub(crate) to: Cell<i32>,
    cur: Cell<i32>,
    start: Cell<i64>,
    tick: RefCell<Option<TickCallbackId>>,
}

impl SizeAnim {
    pub(crate) fn new(widget: &impl IsA<gtk4::Widget>, apply: Rc<dyn Fn(i32)>) -> Rc<Self> {
        Rc::new(Self {
            apply,
            widget: widget.clone().upcast(),
            from: Cell::new(0),
            to: Cell::new(0),
            cur: Cell::new(0),
            start: Cell::new(0),
            tick: RefCell::new(None),
        })
    }

    pub(crate) fn stop(&self) {
        if let Some(id) = self.tick.borrow_mut().take() {
            id.remove();
        }
    }

    pub(crate) fn snap(&self, h: i32) {
        self.stop();
        self.from.set(h);
        self.to.set(h);
        self.cur.set(h);
        (self.apply)(h);
    }

    pub(crate) fn run(self: &Rc<Self>, to: i32) {
        self.stop();
        let from = self.cur.get();
        self.from.set(from);
        self.to.set(to);
        self.start.set(now_ms());
        if from == to {
            return;
        }
        let span = REVEAL_MS as f64;
        let this = self.clone();
        let id = self.widget.add_tick_callback(move |_, _| {
            let p = ((now_ms() - this.start.get()) as f64 / span).clamp(0.0, 1.0);
            let h = this.from.get()
                + ((this.to.get() - this.from.get()) as f64 * ease_out_cubic(p)).round() as i32;
            this.cur.set(h);
            tracing::trace!(h, p, "popup size");
            (this.apply)(h);
            if p >= 1.0 {
                this.tick.borrow_mut().take();
                ControlFlow::Break
            } else {
                ControlFlow::Continue
            }
        });
        *self.tick.borrow_mut() = Some(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curve_starts_at_zero_and_ends_at_one() {
        assert!(ease_out_cubic(0.0).abs() < f64::EPSILON);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn the_curve_is_anchored_at_both_ends_and_fast_at_first() {
        assert!((ease_out_cubic(-1.0) - 0.0).abs() < f64::EPSILON);
        assert!((ease_out_cubic(2.0) - 1.0).abs() < f64::EPSILON);

        assert!(ease_out_cubic(0.5) > 0.8);
    }

    #[test]
    fn the_curve_never_goes_backwards() {
        let mut last = 0.0;
        for i in 0..=100 {
            let p = i as f64 / 100.0;
            let v = ease_out_cubic(p);
            assert!(v >= last, "went backwards at {p}");
            last = v;
        }
    }
}
