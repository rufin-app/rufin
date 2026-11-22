use crate::shell::shell::Shell;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use localization::{tr, trn_with};

use rufin_core::runtime::source::{
    ConfiguredSources, DiscoveredServer, DiscoveryStatus, SourceOperation, SourceSummary,
};

pub(super) mod excluded_folders;
pub(super) mod local_access;
pub mod login;
pub(crate) mod selector;

pub struct SourceState {
    pub configured: RefCell<ConfiguredSources>,
    pub operation: RefCell<SourceOperation>,
    pub discovered_servers: RefCell<Vec<DiscoveredServer>>,
    pub discovery_status: RefCell<DiscoveryStatus>,
    pub discovery_running: Cell<bool>,
    pub discovery_started: Cell<bool>,
    pub discovery_provider: Cell<rufin_core::runtime::source::DiscoveryProvider>,
    /// What the source list's search found; `None` until the first search ends.
    pub found_servers: RefCell<Option<Vec<rufin_core::runtime::source::FoundServer>>>,
    pub finding_servers: Cell<bool>,
    pub add_server: RefCell<Option<login::SourceSetupViewHandle>>,
    pub refresh_feedback_generation: Rc<Cell<u64>>,
    /// The toast up for each Local folder that cannot be read, by the folder's path.
    pub folder_recovery: RefCell<std::collections::HashMap<String, adw::Toast>>,
}

pub(super) fn preference_source_choices(
    shell: &Rc<Shell>,
    button: &gtk::MenuButton,
) -> (Rc<[SourceSummary]>, gtk::SingleSelection) {
    let configured = shell.source.configured.borrow();
    let sources: Rc<[SourceSummary]> = configured.sources.iter().cloned().collect();
    let selected = shell
        .preferences
        .source_choice
        .borrow()
        .clone()
        .or_else(|| configured.selected_source_id.clone());
    let index = sources
        .iter()
        .position(|source| Some(&source.id) == selected.as_ref())
        .unwrap_or(0) as u32;
    let selection =
        crate::widgets::source_labels::bind_source_picker(button, sources.clone(), index, false);
    let weak = Rc::downgrade(shell);
    let choices = sources.clone();
    selection.connect_selected_notify(move |selection| {
        if let (Some(shell), Some(source)) =
            (weak.upgrade(), choices.get(selection.selected() as usize))
        {
            *shell.preferences.source_choice.borrow_mut() = Some(source.id.clone());
        }
    });
    (sources, selection)
}

pub fn half_stars_row(shell: &Rc<Shell>, source: &SourceSummary) -> Option<adw::SwitchRow> {
    if !source.offers_half_star_setting {
        return None;
    }
    let row = adw::SwitchRow::builder()
        .title(tr("Enable partial star ratings"))
        .subtitle(tr(
            "This is not supported natively by this source, but Rufin can show partial stars and then round them up for the source",
        ))
        .active(source.half_stars_enabled)
        .build();
    let source_handle = shell.products.source.clone();
    let source_id = source.id.clone();
    row.connect_active_notify(move |row| {
        source_handle.set_half_stars(source_id.clone(), row.is_active());
    });
    Some(row)
}

/// A file source's switch that writes ratings into its files. Turning it on asks first, and
/// the row follows the source while it shows: its subtitle counts the ratings still to write.
pub fn rating_writes_row(shell: &Rc<Shell>, source: &SourceSummary) -> Option<adw::SwitchRow> {
    use adw::prelude::*;
    use rufin_core::runtime::source::rating_writes_texts;
    if !source.file_library {
        return None;
    }
    let texts = rating_writes_texts(source.ratings_waiting);
    let row = adw::SwitchRow::builder()
        .title(&texts.title)
        .subtitle(&texts.subtitle)
        .active(source.writes_ratings)
        .build();
    let source_handle = shell.products.source.clone();
    let source_id = source.id.clone();
    // Set while the row takes the source's state, which is no choice of the user's.
    let following = Rc::new(Cell::new(false));
    let follows = Rc::clone(&following);
    row.connect_active_notify(move |row| {
        if follows.get() {
            return;
        }
        if !row.is_active() {
            source_handle.set_rating_writes(source_id.clone(), false);
            return;
        }
        let resource = "preferences/dialogs/rating_writes.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, { confirm: adw::AlertDialog });
        let texts = rating_writes_texts(0);
        confirm.set_heading(Some(&texts.heading));
        confirm.set_body(&texts.body);
        confirm.set_response_label("write", &texts.confirm);
        let (weak, follows) = (row.downgrade(), Rc::clone(&follows));
        let (source_handle, source_id) = (source_handle.clone(), source_id.clone());
        gtk::glib::spawn_future_local(async move {
            let Some(row) = weak.upgrade() else { return };
            if confirm.choose_future(Some(&row)).await == "write" {
                source_handle.set_rating_writes(source_id, true);
            } else {
                follows.set(true);
                row.set_active(false);
                follows.set(false);
            }
        });
    });
    let task = Rc::new(RefCell::new(None::<gtk::glib::JoinHandle<()>>));
    let running = Rc::clone(&task);
    let source_handle = shell.products.source.clone();
    let source_id = source.id.clone();
    row.connect_map(move |row| {
        let mut changes = source_handle.configuration_changes();
        let (weak, following) = (row.downgrade(), Rc::clone(&following));
        let (source_handle, source_id) = (source_handle.clone(), source_id.clone());
        let next = gtk::glib::spawn_future_local(async move {
            while changes.changed().await.is_ok() {
                let Some(row) = weak.upgrade() else { return };
                let sources = source_handle.list_sources().sources;
                let Some(source) = sources.iter().find(|source| source.id == source_id) else {
                    continue;
                };
                row.set_subtitle(&rating_writes_texts(source.ratings_waiting).subtitle);
                following.set(true);
                row.set_active(source.writes_ratings);
                following.set(false);
            }
        });
        if let Some(previous) = running.replace(Some(next)) {
            previous.abort();
        }
    });
    row.connect_unmap(move |_| {
        if let Some(task) = task.borrow_mut().take() {
            task.abort();
        }
    });
    Some(row)
}

pub fn playlist_auto_save_row(shell: &Rc<Shell>, source: &SourceSummary) -> Option<adw::SwitchRow> {
    use adw::prelude::*;
    if !source.file_library {
        return None;
    }
    let resource = "preferences/dialogs/playlist_file.ui";
    let row: adw::SwitchRow = crate::ui_resource::object(
        &crate::ui_resource::builder(resource),
        resource,
        "source_auto_save",
    );
    let owner = shell.products.source.clone();
    let source_id = source.id.clone();
    let weak_row = row.downgrade();
    let weak_shell = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let Some(row) = weak_row.upgrade() else {
            return;
        };
        match rufin_core::playlist_files::source_auto_save(&owner, source_id.clone())
            .recv()
            .await
        {
            Ok(Ok(enabled)) => row.set_active(enabled),
            Ok(Err(error)) => {
                if let Some(shell) = weak_shell.upgrade() {
                    shell.control_feedback.show_feedback_toast(error);
                }
                return;
            }
            Err(_) => return,
        }
        row.connect_active_notify(move |row| {
            let result = rufin_core::playlist_files::set_source_auto_save(
                &owner,
                source_id.clone(),
                row.is_active(),
            );
            let weak_shell = weak_shell.clone();
            gtk::glib::spawn_future_local(async move {
                if let Ok(Err(error)) = result.recv().await {
                    if let Some(shell) = weak_shell.upgrade() {
                        shell.control_feedback.show_feedback_toast(error);
                    }
                }
            });
        });
    });
    Some(row)
}

pub fn folder_selected_text(count: u64) -> String {
    let label = count.to_string();
    trn_with(
        "{count} folder selected",
        "{count} folders selected",
        count,
        &[("count", label.as_str())],
    )
}
