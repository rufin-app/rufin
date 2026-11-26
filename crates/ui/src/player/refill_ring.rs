use std::cell::RefCell;
use std::f64::consts::{PI, TAU};

use gtk::{glib, graphene, prelude::*, subclass::prelude::*};

// The ring is drawn this far outside the widget, which keeps the play button's size, and
// it stays visible and allocated while it draws nothing. Appearing is a redraw only: it
// never measures or moves anything.
const OUTSET: f32 = 4.0;
// Leaves a gap as wide as the line between the button and the ring.
const LINE: f64 = 2.0;
// A refill shorter than this, as an ordinary start or seek has, never shows the ring.
const SHOW_AFTER: std::time::Duration = std::time::Duration::from_millis(500);

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct RefillRing {
        // Set while the ring shows: it animates only then, so an idle player does not
        // wake every frame.
        pub(super) tick: RefCell<Option<gtk::TickCallbackId>>,
        pub(super) pending: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RefillRing {
        const NAME: &'static str = "RufinRefillRing";
        type Type = super::RefillRing;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for RefillRing {}

    impl WidgetImpl for RefillRing {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if self.tick.borrow().is_none() {
                return;
            }
            let ring = self.obj();
            let (width, height) = (ring.width() as f32, ring.height() as f32);
            let color = ring.color();
            let (red, green, blue, alpha) = (
                f64::from(color.red()),
                f64::from(color.green()),
                f64::from(color.blue()),
                f64::from(color.alpha()),
            );
            let seconds = ring.frame_clock().map_or(0, |clock| clock.frame_time()) as f64 / 1e6;
            let (head, tail) = spinner_arc(seconds);
            let cr = snapshot.append_cairo(&graphene::Rect::new(
                -OUTSET,
                -OUTSET,
                width + OUTSET * 2.0,
                height + OUTSET * 2.0,
            ));
            let (x, y) = (f64::from(width) / 2.0, f64::from(height) / 2.0);
            let radius = x.min(y) + f64::from(OUTSET) - LINE / 2.0;
            cr.set_line_width(LINE);
            cr.set_line_cap(gtk::cairo::LineCap::Round);
            cr.set_source_rgba(red, green, blue, alpha * 0.15);
            cr.arc(x, y, radius, 0.0, TAU);
            let _ = cr.stroke();
            cr.set_source_rgba(red, green, blue, alpha);
            cr.arc(x, y, radius, tail, head);
            let _ = cr.stroke();
        }
    }
}

glib::wrapper! {
    /// A thin ring around the play button that moves like AdwSpinner while a stream
    /// refills. AdwSpinner itself widens its stroke with its size, which would make this
    /// a thick band.
    pub struct RefillRing(ObjectSubclass<imp::RefillRing>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl RefillRing {
    /// A ring around a circular widget of `size`.
    pub fn new(size: i32) -> Self {
        let ring: Self = glib::Object::new();
        ring.set_size_request(size, size);
        ring.set_can_target(false);
        ring.add_css_class("player-refill-ring");
        ring
    }

    /// Shows the ring once a refill has lasted long enough to notice, and hides it when it ends.
    pub fn set_refilling(&self, refilling: bool) {
        let imp = self.imp();
        if !refilling {
            if let Some(source) = imp.pending.take() {
                source.remove();
            }
            if let Some(tick) = imp.tick.take() {
                tick.remove();
                self.queue_draw();
            }
        } else if imp.tick.borrow().is_none() && imp.pending.borrow().is_none() {
            let ring = self.downgrade();
            let source = glib::timeout_add_local_once(SHOW_AFTER, move || {
                if let Some(ring) = ring.upgrade() {
                    ring.imp().pending.take();
                    let tick = ring.add_tick_callback(|ring, _| {
                        ring.queue_draw();
                        glib::ControlFlow::Continue
                    });
                    ring.imp().tick.replace(Some(tick));
                }
            });
            imp.pending.replace(Some(source));
        }
    }
}

/// AdwSpinnerPaintable's arc: the head extends, then the tail follows, while the whole
/// turns once every 1.2 seconds. Returns the head and tail angles.
fn spinner_arc(seconds: f64) -> (f64, f64) {
    const IDLE: f64 = PI * 0.9;
    const OVERLAP: f64 = PI * 0.7;
    const EXTEND: f64 = PI * 1.1;
    const CONTRACT: f64 = PI * 1.35;
    const MIN_ARC: f64 = PI * 0.015;
    const MAX_ARC: f64 = PI * 0.9;
    const START: f64 = PI * 0.35;
    let cycle = IDLE + EXTEND + CONTRACT - OVERLAP;
    let ease = |t: f64| (1.0 - (PI * t).cos()) / 2.0;
    let base = seconds / 1.2 * TAU;
    let angle = base % cycle;
    let extended = if angle > EXTEND {
        1.0
    } else {
        ease(angle / EXTEND)
    };
    let contracted = if angle < EXTEND - OVERLAP {
        0.0
    } else if angle > cycle - IDLE {
        1.0
    } else {
        ease((angle - EXTEND + OVERLAP) / CONTRACT)
    };
    let drift = angle * MAX_ARC / cycle;
    (
        base + MIN_ARC + (MAX_ARC - MIN_ARC) * extended - drift + START,
        base + (MAX_ARC - MIN_ARC) * contracted - drift + START,
    )
}
