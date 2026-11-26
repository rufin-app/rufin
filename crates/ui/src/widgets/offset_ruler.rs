//! The lyrics offset ruler: a tape of millisecond ticks that slides under a fixed needle.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use gtk::{glib, prelude::*, subclass::prelude::*};

/// Milliseconds one pixel of the tape stands for.
const MILLIS_PER_PIXEL: f64 = 5.0;
const TICK_MILLIS: i64 = 50;
const LABEL_MILLIS: i64 = 250;
/// What a drag, an arrow key and a wheel step move the value by.
const DRAG_STEP_MILLIS: i64 = 10;
const WHEEL_STEP_MILLIS: i64 = TICK_MILLIS;
const NEEDLE_WIDTH: i32 = 4;
const NEEDLE_HEIGHT: i32 = 30;
const HEIGHT: i32 = 44;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct OffsetRuler {
        pub(super) value: Cell<i64>,
        pub(super) drag_start: Cell<i64>,
        pub(super) changed: RefCell<Option<Rc<dyn Fn(i64)>>>,
        pub(super) needle: OnceCell<gtk::Box>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OffsetRuler {
        const NAME: &'static str = "RufinOffsetRuler";
        type Type = super::OffsetRuler;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_css_name("offset-ruler");
            class.set_accessible_role(gtk::AccessibleRole::Slider);
        }
    }

    impl ObjectImpl for OffsetRuler {
        fn constructed(&self) {
            self.parent_constructed();
            let ruler = self.obj();
            ruler.set_focusable(true);
            ruler.set_focus_on_click(true);
            ruler.set_hexpand(true);
            ruler.set_cursor_from_name(Some("ew-resize"));
            let needle = gtk::Box::new(gtk::Orientation::Vertical, 0);
            needle.add_css_class("offset-ruler-needle");
            needle.set_can_target(false);
            needle.set_parent(&*ruler);
            self.needle.set(needle).expect("needle is set once");

            let drag = gtk::GestureDrag::new();
            let weak = ruler.downgrade();
            drag.connect_drag_begin(move |_, _, _| {
                if let Some(ruler) = weak.upgrade() {
                    ruler.grab_focus();
                    ruler.imp().drag_start.set(ruler.imp().value.get());
                }
            });
            let weak = ruler.downgrade();
            drag.connect_drag_update(move |_, x, _| {
                if let Some(ruler) = weak.upgrade() {
                    // The tape follows the pointer, so the value under the needle moves against it.
                    let steps = (-x * MILLIS_PER_PIXEL / DRAG_STEP_MILLIS as f64).round() as i64;
                    ruler.change(
                        ruler
                            .imp()
                            .drag_start
                            .get()
                            .saturating_add(steps * DRAG_STEP_MILLIS),
                    );
                }
            });
            ruler.add_controller(drag);

            let scroll = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::BOTH_AXES
                    | gtk::EventControllerScrollFlags::DISCRETE,
            );
            let weak = ruler.downgrade();
            scroll.connect_scroll(move |_, x, y| {
                let Some(ruler) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let steps = if y == 0.0 { x } else { -y }.round() as i64;
                ruler.step(steps * WHEEL_STEP_MILLIS);
                glib::Propagation::Stop
            });
            ruler.add_controller(scroll);

            let keys = gtk::EventControllerKey::new();
            let weak = ruler.downgrade();
            keys.connect_key_pressed(move |_, key, _, _| {
                let Some(ruler) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let step = match key {
                    gtk::gdk::Key::Left | gtk::gdk::Key::KP_Left => -DRAG_STEP_MILLIS,
                    gtk::gdk::Key::Right | gtk::gdk::Key::KP_Right => DRAG_STEP_MILLIS,
                    _ => return glib::Propagation::Proceed,
                };
                ruler.step(step);
                glib::Propagation::Stop
            });
            ruler.add_controller(keys);
        }

        fn dispose(&self) {
            if let Some(needle) = self.needle.get() {
                needle.unparent();
            }
        }
    }

    impl WidgetImpl for OffsetRuler {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (120, 240, -1, -1),
                _ => (HEIGHT, HEIGHT, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let needle = self.needle.get().expect("needle");
            needle.measure(gtk::Orientation::Horizontal, -1);
            needle.allocate(
                NEEDLE_WIDTH,
                NEEDLE_HEIGHT,
                -1,
                Some(
                    gtk::gsk::Transform::new().translate(&gtk::graphene::Point::new(
                        ((width - NEEDLE_WIDTH) / 2) as f32,
                        0.0,
                    )),
                ),
            );
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let ruler = self.obj();
            let (width, height) = (ruler.width() as f32, ruler.height() as f32);
            if width <= 0.0 || height <= 0.0 {
                return;
            }
            let foreground = ruler.color();
            let shade = |alpha: f32| {
                let mut color = foreground;
                color.set_alpha(alpha);
                color
            };
            let value = self.value.get() as f64;
            let center = f64::from(width) / 2.0;
            let layout = ruler.create_pango_layout(None);
            let mut font = gtk::pango::FontDescription::new();
            font.set_absolute_size(11.0 * gtk::pango::SCALE as f64);
            layout.set_font_description(Some(&font));

            // The tape fades out towards both ends.
            crate::widgets::edge_fade::push_mask(
                snapshot,
                width,
                height,
                false,
                width * 0.18,
                width * 0.18,
            );
            let first = ((value - center * MILLIS_PER_PIXEL) / TICK_MILLIS as f64).floor() as i64;
            let last = ((value + center * MILLIS_PER_PIXEL) / TICK_MILLIS as f64).ceil() as i64;
            for tick in first..=last {
                let millis = tick * TICK_MILLIS;
                let x = (center + (millis as f64 - value) / MILLIS_PER_PIXEL) as f32;
                let labelled = millis % LABEL_MILLIS == 0;
                let (thickness, length, alpha) = if labelled {
                    (2.0, 16.0, 1.0)
                } else {
                    (1.0, 8.0, 0.5)
                };
                snapshot.append_color(
                    &shade(alpha),
                    &gtk::graphene::Rect::new(x - thickness / 2.0, 0.0, thickness, length),
                );
                if labelled {
                    layout.set_text(&if millis == 0 {
                        "0".to_string()
                    } else {
                        format!("{millis:+}")
                    });
                    snapshot.save();
                    snapshot.translate(&gtk::graphene::Point::new(
                        x - layout.pixel_size().0 as f32 / 2.0,
                        22.0,
                    ));
                    snapshot.append_layout(&layout, &shade(0.7));
                    snapshot.restore();
                }
            }
            snapshot.pop();
            ruler.snapshot_child(self.needle.get().expect("needle"), snapshot);
        }
    }
}

glib::wrapper! {
    pub struct OffsetRuler(ObjectSubclass<imp::OffsetRuler>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl OffsetRuler {
    /// Shows `millis` under the needle without reporting a change.
    pub fn set_value(&self, millis: i64) {
        if self.imp().value.replace(millis) != millis {
            self.queue_draw();
        }
        self.update_property(&[
            gtk::accessible::Property::ValueNow(millis as f64),
            gtk::accessible::Property::ValueText(&format!("{millis} ms")),
        ]);
    }

    pub fn value(&self) -> i64 {
        self.imp().value.get()
    }

    /// `changed` gets every value the user moves the ruler to.
    pub fn connect_changed(&self, changed: impl Fn(i64) + 'static) {
        self.imp().changed.replace(Some(Rc::new(changed)));
    }

    fn step(&self, millis: i64) {
        self.change(self.imp().value.get().saturating_add(millis));
    }

    fn change(&self, millis: i64) {
        if self.imp().value.get() == millis {
            return;
        }
        self.set_value(millis);
        let changed = self.imp().changed.borrow().clone();
        if let Some(changed) = changed {
            changed(millis);
        }
    }
}
