//! The row each kind of metadata field edits its value in. The value stays the field's text:
//! `a; b`, a date as its source writes it.

use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use localization::tr;
use sources::MetadataFieldKind;

pub(super) enum Control {
    /// Text, dates and fields chosen from a vocabulary.
    Entry(adw::EntryRow),
    Number(adw::ActionRow, gtk::SpinButton),
    List(Rc<Values>),
    /// The stars, and whether they take half stars.
    Rating(adw::ActionRow, crate::ratings::RatingControl, bool),
}

impl Control {
    /// The row of a number, a list or a rating; the other kinds edit text in an entry row.
    pub(super) fn of_field(
        field: &sources::MetadataField,
    ) -> Option<(Self, gtk::Button, gtk::Button)> {
        let kind = field.kind;
        let resource = match kind {
            MetadataFieldKind::Number => "preferences/dialogs/metadata_number_field.ui",
            MetadataFieldKind::List => "preferences/dialogs/metadata_list_field.ui",
            MetadataFieldKind::Rating => "preferences/dialogs/metadata_rating_field.ui",
            MetadataFieldKind::Text | MetadataFieldKind::Date | MetadataFieldKind::Boolean => {
                return None;
            }
        };
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, { clear: gtk::Button, undo: gtk::Button });
        let control = match kind {
            MetadataFieldKind::Rating => {
                crate::objects!(builder, resource, { row: adw::ActionRow, stars: gtk::Box });
                // Its options are the ratings it takes: an odd one is a half star.
                let half_stars = field.options.iter().any(|rating| rating == "1");
                let rating = crate::ratings::RatingControl::new(None, half_stars);
                stars.append(rating.widget());
                Self::Rating(row, rating, half_stars)
            }
            MetadataFieldKind::Number => {
                crate::objects!(builder, resource, { row: adw::ActionRow, spin: gtk::SpinButton });
                // No number is an empty row, not a zero.
                spin.connect_output(|spin| {
                    let empty = spin.value() == 0.0;
                    if empty {
                        spin.set_text("");
                    }
                    gtk::glib::Propagation::from(empty)
                });
                spin.connect_input(|spin| spin.text().is_empty().then_some(Ok(0.0)));
                spin.update();
                Self::Number(row, spin)
            }
            _ => {
                crate::objects!(builder, resource, {
                    row: adw::PreferencesRow,
                    values: adw::WrapBox,
                    add: gtk::Text,
                });
                Self::List(Values::new(row, values, add))
            }
        };
        Some((control, clear, undo))
    }

    pub(super) fn row(&self) -> adw::PreferencesRow {
        match self {
            Self::Entry(row) => row.clone().upcast(),
            Self::Number(row, _) => row.clone().upcast(),
            Self::List(values) => values.row.clone(),
            Self::Rating(row, _, _) => row.clone().upcast(),
        }
    }

    pub(super) fn set_text(&self, text: &str) {
        match self {
            Self::Entry(row) if row.text() != text => row.set_text(text),
            Self::Entry(_) => {}
            Self::Number(_, spin) => {
                // The number a text starts with: tags hold `8; 8` and `120.5` too.
                let number = text
                    .split(|digit: char| !digit.is_ascii_digit())
                    .next()
                    .unwrap_or_default();
                if spin.text() != number {
                    spin.set_text(number);
                    spin.update();
                }
            }
            Self::List(values) => values.set_text(text),
            Self::Rating(_, rating, half_stars) => {
                rating.set_rating(text.parse().ok(), *half_stars);
            }
        }
    }

    /// Calls `changed` with the row's text each time it changes.
    pub(super) fn connect_changed(&self, changed: impl Fn(&str) + 'static) {
        match self {
            Self::Entry(row) => {
                row.connect_changed(move |row| changed(&row.text()));
            }
            Self::Number(_, spin) => {
                spin.connect_changed(move |spin| changed(&spin.text()));
            }
            Self::List(values) => values.changed.borrow_mut().push(Rc::new(changed)),
            Self::Rating(_, rating, _) => rating.connect_commit(move |rating| {
                changed(&rating.map(|rating| rating.to_string()).unwrap_or_default());
            }),
        }
    }
}

fn split(text: &str) -> impl Iterator<Item = &str> {
    text.split(';')
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// A list's values, each a chip with its own remove button, and the text that adds one.
pub(super) struct Values {
    row: adw::PreferencesRow,
    values: adw::WrapBox,
    add: gtk::Text,
    chips: RefCell<Vec<(String, gtk::Box)>>,
    changed: RefCell<Vec<Rc<dyn Fn(&str)>>>,
}

impl Values {
    fn new(row: adw::PreferencesRow, values: adw::WrapBox, add: gtk::Text) -> Rc<Self> {
        let list = Rc::new(Self {
            row,
            values,
            add,
            chips: RefCell::default(),
            changed: RefCell::default(),
        });
        // The text being typed counts as a value already, so a save never drops it.
        let weak = Rc::downgrade(&list);
        list.add.connect_changed(move |add| {
            let Some(list) = weak.upgrade() else { return };
            let text = add.text();
            match text.rsplit_once(';') {
                Some((done, rest)) => {
                    list.push(done);
                    add.set_text(rest.trim_start());
                }
                None => list.emit(),
            }
        });
        let weak = Rc::downgrade(&list);
        list.add.connect_activate(move |_| {
            if let Some(list) = weak.upgrade() {
                list.finish();
            }
        });
        let weak = Rc::downgrade(&list);
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave(move |_| {
            if let Some(list) = weak.upgrade() {
                list.finish();
            }
        });
        list.add.add_controller(focus);
        list
    }

    fn text(&self) -> String {
        let add = self.add.text();
        self.chips
            .borrow()
            .iter()
            .map(|(value, _)| value.as_str())
            .chain(split(&add))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn set_text(self: &Rc<Self>, text: &str) {
        if self.text() == split(text).collect::<Vec<_>>().join("; ") {
            return;
        }
        for (_, chip) in self.chips.take() {
            self.values.remove(&chip);
        }
        self.add.set_text("");
        self.push(text);
    }

    /// Turns the typed text into a chip.
    fn finish(self: &Rc<Self>) {
        let text = self.add.text();
        self.push(&text);
        self.add.set_text("");
    }

    fn push(self: &Rc<Self>, text: &str) {
        for value in split(text) {
            let chip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            chip.add_css_class("metadata-chip");
            let label = gtk::Label::new(Some(value));
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            let remove = gtk::Button::from_icon_name("rufin-window-close-symbolic");
            remove.add_css_class("flat");
            remove.add_css_class("circular");
            remove.set_tooltip_text(Some(&tr("Remove")));
            remove.update_property(&[gtk::accessible::Property::Label(&tr("Remove"))]);
            chip.append(&label);
            chip.append(&remove);
            let (weak, weak_chip) = (Rc::downgrade(self), chip.downgrade());
            remove.connect_clicked(move |_| {
                let (Some(list), Some(chip)) = (weak.upgrade(), weak_chip.upgrade()) else {
                    return;
                };
                list.chips.borrow_mut().retain(|(_, other)| *other != chip);
                list.values.remove(&chip);
                list.emit();
            });
            let last = self.chips.borrow().last().map(|(_, chip)| chip.clone());
            self.values.insert_child_after(&chip, last.as_ref());
            self.chips.borrow_mut().push((value.to_owned(), chip));
        }
    }

    fn emit(&self) {
        let text = self.text();
        let changed = self.changed.borrow().clone();
        for changed in changed {
            changed(&text);
        }
    }
}

/// A date is typed, in whole or in part, or picked from the calendar this adds to its row.
pub(super) fn connect_calendar(entry: &adw::EntryRow, choose: &gtk::MenuButton) {
    choose.set_visible(true);
    let weak_entry = entry.downgrade();
    // Each opening is a new calendar on the row's date. A calendar keeps its focus outline on
    // the cell last clicked, whichever date it is given later.
    choose.set_create_popup_func(move |choose| {
        let Some(entry) = weak_entry.upgrade() else {
            return;
        };
        let resource = "preferences/dialogs/metadata_date.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, { popover: gtk::Popover, calendar: gtk::Calendar });
        let text = entry.text();
        let mut parts = text
            .split(|digit: char| !digit.is_ascii_digit())
            .map(|part| part.parse::<i32>().ok());
        // An empty row leaves the calendar on today; a year or a month opens on its first day.
        if let Some(Some(year)) = parts.next() {
            let mut part = || parts.next().flatten().unwrap_or(1);
            let (month, day) = (part(), part());
            if let Ok(date) = gtk::glib::DateTime::from_local(year, month, day, 0, 0, 0.0) {
                calendar.set_date(&date);
            }
        }
        let (weak_entry, weak_choose) = (entry.downgrade(), choose.downgrade());
        calendar.connect_day_selected(move |calendar| {
            if let (Some(entry), Ok(date)) =
                (weak_entry.upgrade(), calendar.date().format("%Y-%m-%d"))
            {
                entry.set_text(&date);
            }
            if let Some(choose) = weak_choose.upgrade() {
                choose.popdown();
            }
        });
        choose.set_popover(Some(&popover));
    });
}
