use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::gio::prelude::FileExtManual;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};
use localization::tr;
use rufin_core::diagnostics::{DiagnosticEntry, DiagnosticLevel};
use tracing::warn;

use crate::widgets::layout::{large_popup_content_height, large_popup_content_width};
use crate::widgets::popup::present_light_dismiss_dialog;

use super::Shell;

const DIAGNOSTICS_DIALOG_WIDTH: i32 = 680;
const DIAGNOSTICS_DIALOG_HEIGHT: i32 = 560;
const LOG_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
/// An entry of the list, and whether its row shows all of it.
struct LogItem {
    entry: DiagnosticEntry,
    open: Cell<bool>,
}

crate::composite_box!(
    pub LogRow,
    log_row_imp,
    "RufinLogRow",
    "/app/rufin/rufin/ui/shell/diagnostics_row.ui",
    { time: gtk::Label, level: gtk::Label, origin: gtk::Label, message: gtk::Label, fields: gtk::Label }
);

pub(super) fn present_diagnostics(shell: &Rc<Shell>) {
    let resource = "shell/diagnostics.ui";
    let builder = crate::ui_resource::builder(resource);
    let dialog: adw::Dialog = crate::ui_resource::object(&builder, resource, "diagnostics_dialog");
    dialog.set_content_width(large_popup_content_width(DIAGNOSTICS_DIALOG_WIDTH));
    dialog.set_content_height(large_popup_content_height(
        shell.chrome.window.height(),
        DIAGNOSTICS_DIALOG_HEIGHT,
    ));
    let debug: adw::SwitchRow = crate::ui_resource::object(&builder, resource, "debug");
    debug.set_active(shell.diagnostics.debug_enabled());

    let log: gtk::ListView = crate::ui_resource::object(&builder, resource, "log");
    log.update_property(&[gtk::accessible::Property::Label(&tr("Diagnostic log"))]);
    let scroller: gtk::ScrolledWindow =
        crate::ui_resource::object(&builder, resource, "log_scroller");
    let levels: adw::ToggleGroup = crate::ui_resource::object(&builder, resource, "levels");
    let copy: gtk::Button = crate::ui_resource::object(&builder, resource, "copy");
    let save: gtk::Button = crate::ui_resource::object(&builder, resource, "save");
    let status: gtk::Label = crate::ui_resource::object(&builder, resource, "status");
    drop(builder);

    let changing_debug = Rc::new(Cell::new(false));
    let diagnostics = shell.diagnostics.clone();
    let debug_status = status.clone();
    let changing_debug_for_notify = Rc::clone(&changing_debug);
    debug.connect_active_notify(move |row| {
        if changing_debug_for_notify.get() {
            return;
        }
        let enabled = row.is_active();
        if let Err(error) = diagnostics.set_debug_enabled(enabled) {
            changing_debug_for_notify.set(true);
            row.set_active(!enabled);
            changing_debug_for_notify.set(false);
            warn!(%error, "could not change debug logging");
            debug_status.add_css_class("error");
            debug_status.set_text(&error);
            debug_status.set_visible(true);
        }
    });

    // The list builds a row only for an entry in view, so the whole log costs what its text does.
    let entries = gio::ListStore::new::<glib::BoxedAnyObject>();
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().unwrap();
        item.set_activatable(false);
        let row = LogRow::new();
        // A click opens the row to its whole entry, and closes it again.
        let click = gtk::GestureClick::new();
        let item_weak = item.downgrade();
        click.connect_released(move |_, _, _, _| {
            let Some(item) = item_weak.upgrade() else {
                return;
            };
            let (Some(row), Some(entry)) = (
                item.child().and_downcast::<LogRow>(),
                item.item().and_downcast::<glib::BoxedAnyObject>(),
            ) else {
                return;
            };
            let entry = entry.borrow::<LogItem>();
            entry.open.set(!entry.open.get());
            row.show(&entry.entry, entry.open.get());
        });
        row.add_controller(click);
        item.set_child(Some(&row));
    });
    factory.connect_bind(|_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().unwrap();
        let row = item.child().and_downcast::<LogRow>().unwrap();
        let entry = item.item().and_downcast::<glib::BoxedAnyObject>().unwrap();
        let entry = entry.borrow::<LogItem>();
        row.show(&entry.entry, entry.open.get());
    });
    log.set_model(Some(&gtk::NoSelection::new(Some(entries.clone()))));
    log.set_factory(Some(&factory));
    let cursor = Rc::new(Cell::new(0));
    let refresh = Rc::new({
        let diagnostics = shell.diagnostics.clone();
        let (log, scroller, entries) = (log.clone(), scroller.clone(), entries.clone());
        let cursor = Rc::clone(&cursor);
        let levels = levels.clone();
        move || {
            let update = diagnostics.updates(cursor.get());
            if update.end == cursor.get() {
                return;
            }
            let adjustment = scroller.vadjustment();
            let follows_tail =
                adjustment.value() + adjustment.page_size() >= adjustment.upper() - 24.0;
            // Entries the log no longer keeps lead the list.
            let dropped = (0..entries.n_items())
                .take_while(|index| {
                    entries
                        .item(*index)
                        .and_downcast::<glib::BoxedAnyObject>()
                        .is_some_and(|entry| {
                            entry.borrow::<LogItem>().entry.position < update.start
                        })
                })
                .count() as u32;
            let picked = match levels.active_name().as_deref() {
                Some("warnings") => Some(DiagnosticLevel::Warning),
                Some("errors") => Some(DiagnosticLevel::Error),
                _ => None,
            };
            let added: Vec<_> = update
                .entries
                .into_iter()
                .filter(|entry| picked.is_none() || entry.level == picked)
                .map(|entry| {
                    glib::BoxedAnyObject::new(LogItem {
                        entry,
                        open: Cell::new(false),
                    })
                })
                .collect();
            entries.splice(0, dropped, &[] as &[glib::BoxedAnyObject]);
            entries.extend_from_slice(&added);
            cursor.set(update.end);
            if follows_tail && entries.n_items() > 0 {
                log.scroll_to(entries.n_items() - 1, gtk::ListScrollFlags::NONE, None);
            }
        }
    });
    refresh();
    {
        let (entries, cursor) = (entries.clone(), Rc::clone(&cursor));
        let refresh = Rc::clone(&refresh);
        levels.connect_active_name_notify(move |_| {
            entries.remove_all();
            cursor.set(0);
            refresh();
        });
    }
    let timer = glib::timeout_add_local(LOG_REFRESH_INTERVAL, move || {
        refresh();
        glib::ControlFlow::Continue
    });
    let timer = RefCell::new(Some(timer));
    dialog.connect_closed(move |_| {
        if let Some(timer) = timer.borrow_mut().take() {
            timer.remove();
        }
    });

    let copy_diagnostics = shell.diagnostics.clone();
    let copy_status = status.clone();
    copy.connect_clicked(move |button| {
        button
            .display()
            .clipboard()
            .set_text(&copy_diagnostics.snapshot());
        copy_status.remove_css_class("error");
        copy_status.set_text(&tr("Logs are copied"));
    });

    let save_window = shell.chrome.window.clone();
    let save_diagnostics = shell.diagnostics.clone();
    let save_status = status.clone();
    save.connect_clicked(move |_| {
        let window = save_window.clone();
        let diagnostics = save_diagnostics.clone();
        let status = save_status.clone();
        glib::spawn_future_local(async move {
            let chooser = gtk::FileDialog::builder()
                .title(tr("Save Diagnostic Log"))
                .initial_name("rufin-debug.log")
                .build();
            let Ok(file) = chooser.save_future(Some(&window)).await else {
                return;
            };
            let contents = diagnostics.snapshot().into_bytes();
            match file
                .replace_contents_future(
                    contents,
                    None,
                    false,
                    gio::FileCreateFlags::REPLACE_DESTINATION,
                )
                .await
            {
                Ok(_) => {
                    status.remove_css_class("error");
                    status.set_text(&tr("Diagnostic log saved"));
                    status.set_visible(true);
                }
                Err((_, error)) => {
                    warn!(%error, "could not save diagnostic log");
                    status.add_css_class("error");
                    status.set_text(&format!("{}: {error}", tr("Could not save diagnostic log")));
                    status.set_visible(true);
                }
            }
        });
    });

    present_light_dismiss_dialog(&dialog, &shell.chrome.window);
}

impl LogRow {
    /// An entry as a row: when and how serious on its left, then where from, the message and its
    /// values, cut short until the row is opened. Text that is not an entry of Rufin's own fills the
    /// row in monospace.
    fn show(&self, entry: &DiagnosticEntry, open: bool) {
        let row = self.imp();
        // A closed row stops at the lines its labels are given; the list lays out a few hundred rows
        // around the ones in view, and whole entries of a debug log made that take seconds.
        let cut = if open {
            gtk::pango::EllipsizeMode::None
        } else {
            gtk::pango::EllipsizeMode::End
        };
        row.message.set_ellipsize(cut);
        row.fields.set_ellipsize(cut);
        let logged = entry.level.is_some();
        row.time.set_visible(logged);
        row.origin.set_visible(logged);
        row.time.set_label(
            &glib::DateTime::from_unix_local((entry.time_millis / 1000) as i64)
                .and_then(|time| time.format("%H:%M:%S"))
                .unwrap_or_default(),
        );
        let (name, class) = match entry.level {
            Some(DiagnosticLevel::Error) => ("ERROR", Some("log-error")),
            Some(DiagnosticLevel::Warning) => ("WARN", Some("log-warning")),
            Some(DiagnosticLevel::Debug) => ("DEBUG", Some("log-quiet")),
            Some(DiagnosticLevel::Trace) => ("TRACE", Some("log-quiet")),
            Some(DiagnosticLevel::Info) | None => ("", None),
        };
        row.level.set_label(name);
        row.level.set_visible(!name.is_empty());
        for known in ["log-error", "log-warning", "log-quiet", "log-raw"] {
            self.remove_css_class(known);
        }
        if let Some(class) = class.or((!logged).then_some("log-raw")) {
            self.add_css_class(class);
        }
        row.origin.set_label(&entry.origin);
        row.message.set_label(&entry.message);
        row.message.set_visible(!entry.message.is_empty());
        row.fields.set_label(&entry.fields);
        row.fields.set_visible(!entry.fields.is_empty());
    }
}
