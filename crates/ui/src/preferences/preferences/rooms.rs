//! The Rooms button of the top bar and its popover: creating or joining a room, and who is in
//! the room this device listens in.
use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use localization::{tr, tr_with};
use rufin_core::room::{RoomAction as Action, RoomStatus as Status};

use crate::shell::shell::Shell;

const ICON: &str = "rufin-people-symbolic";

/// The popover's parts, once it was opened.
struct Shown {
    pages: gtk::Stack,
    name: adw::EntryRow,
    create: gtk::Button,
    reach: adw::ActionRow,
    invite: adw::EntryRow,
    join: gtk::Button,
    failure: gtk::Label,
    room_group: adw::PreferencesGroup,
    room: adw::ActionRow,
    member_rows: RefCell<Vec<adw::ActionRow>>,
    share: adw::SwitchRow,
    room_reach: adw::ActionRow,
    use_relay: gtk::Button,
    leave: gtk::Button,
}

/// Asks before the host ends the room for everyone. The question opens over the window, so
/// it outlasts the popover `from` is in.
fn confirm_end(shell: &Rc<Shell>, from: &gtk::Button) {
    let dialog = adw::AlertDialog::builder()
        .heading(tr("End the room?"))
        .body(tr("Everyone stops listening together"))
        .build();
    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("end", &tr("End room"));
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("end", adw::ResponseAppearance::Destructive);
    let weak = Rc::downgrade(shell);
    dialog.choose(
        from.root().as_ref(),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            if response.as_str() == "end"
                && let Some(shell) = weak.upgrade()
            {
                run(&shell, Action::Leave, None);
            }
        },
    );
}

/// An invite link was opened: asks whether to join its room, and under which name.
fn ask_join(shell: &Rc<Shell>, over: &gtk::MenuButton, invite: &str) {
    let owner = &shell.products.room;
    let room = owner.invite_room(invite.into());
    if room.is_empty() {
        (shell.control_feedback).show_feedback_toast(tr("This is not a room invite"));
        return;
    }
    let name = adw::EntryRow::builder()
        .title(tr("Your name"))
        .text(owner.status().your_name)
        .activates_default(true)
        .build();
    let named = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    named.append(&name);
    let dialog = adw::AlertDialog::builder()
        .heading(tr_with("Join {room}?", &[("room", &room)]))
        .extra_child(&named)
        .build();
    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("join", &tr("Join"));
    dialog.set_default_response(Some("join"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("join", adw::ResponseAppearance::Suggested);
    let (weak, invite) = (Rc::downgrade(shell), invite.to_owned());
    dialog.choose(
        over.root().as_ref(),
        None::<&gtk::gio::Cancellable>,
        move |response| {
            if response.as_str() == "join"
                && let Some(shell) = weak.upgrade()
            {
                let name = name.text().into();
                run(&shell, Action::Join { invite, name }, None);
            }
        },
    );
}

fn run(shell: &Rc<Shell>, action: Action, failure: Option<gtk::Label>) {
    let owner = shell.products.room.clone();
    let result = shell
        .products
        .runtime
        .spawn(async move { owner.execute(action).await });
    let weak = Rc::downgrade(shell);
    gtk::glib::spawn_future_local(async move {
        let failed = match result.await {
            Ok(Ok(_)) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(error) => Some(error.to_string()),
        };
        match (failed, failure) {
            (failed, Some(label)) => {
                label.set_visible(failed.is_some());
                label.set_text(failed.as_deref().unwrap_or_default());
            }
            (Some(failed), None) => {
                if let Some(shell) = weak.upgrade() {
                    shell.control_feedback.show_feedback_toast(failed);
                }
            }
            (None, None) => {}
        }
    });
}

/// A listener or a room as a row: its name, a line under it and its initial.
fn named_row(name: &str, line: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title(name)
        .subtitle(line)
        .title_lines(1)
        .build();
    row.add_prefix(&adw::Avatar::new(28, Some(name), true));
    row
}

/// Puts `rows` in `group` in place of the ones it held.
fn replace_rows(
    group: &adw::PreferencesGroup,
    held: &RefCell<Vec<adw::ActionRow>>,
    rows: Vec<adw::ActionRow>,
) {
    for row in held.take() {
        group.remove(&row);
    }
    for row in &rows {
        group.add(row);
    }
    held.replace(rows);
}

fn show(shell: &Rc<Shell>, shown: &Shown, status: &Status) {
    let listening = |count: usize| tr_with("{count} listening", &[("count", &count.to_string())]);
    let Some(room) = &status.room else {
        shown.pages.set_visible_child_name("start");
        if shown.name.text().is_empty() {
            shown.name.set_text(&status.your_name);
        }
        shown.reach.set_title(&status.reach);
        // A join under way is given up from where it was started.
        let joining = status.joining;
        (shown.join).set_label(&if joining { tr("Cancel") } else { tr("Join") });
        (shown.join).set_sensitive(joining || !shown.invite.text().trim().is_empty());
        // The button is the row's own: only the link is held still.
        shown.invite.set_editable(!joining);
        shown.create.set_sensitive(!joining);
        return;
    };
    shown.pages.set_visible_child_name("room");
    shown.failure.set_visible(false);
    shown.invite.set_text("");
    shown.room.set_title(&room.name);
    shown.room.set_subtitle(&listening(room.members.len()));
    let leave = if room.hosting {
        tr("End room")
    } else {
        tr("Leave room")
    };
    shown.leave.set_tooltip_text(Some(&leave));
    (shown.leave).update_property(&[gtk::accessible::Property::Label(&leave)]);
    // Everyone sees whether the room shares songs; the host decides it.
    shown.share.set_sensitive(room.hosting);
    shown.share.set_active(room.sharing);
    shown.room_reach.set_title(&room.reach);
    shown.use_relay.set_visible(room.relay_offered);
    let rows = (room.members.iter())
        .map(|member| {
            let line = if member.sitting_out {
                tr("Sitting this one out")
            } else {
                tr("Listening")
            };
            let row = named_row(&member.name, &line);
            let tag = match (member.host, member.you) {
                (true, _) => Some(tr("Host")),
                (false, true) => Some(tr("You")),
                _ => None,
            };
            if let Some(tag) = tag {
                let tag = gtk::Label::new(Some(&tag));
                tag.add_css_class("dim-label");
                row.add_suffix(&tag);
            }
            if room.hosting && !member.host {
                let remove = gtk::Button::builder()
                    .icon_name(crate::widgets::controls::REMOVE_ICON)
                    .tooltip_text(tr("Remove from room"))
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build();
                let (weak, id) = (Rc::downgrade(shell), member.id.clone());
                remove.connect_clicked(move |_| {
                    if let Some(shell) = weak.upgrade() {
                        run(&shell, Action::Remove { member: id.clone() }, None);
                    }
                });
                row.add_suffix(&remove);
            }
            row
        })
        .collect();
    replace_rows(&shown.room_group, &shown.member_rows, rows);
}

fn popover(shell: &Rc<Shell>, shared: &Rc<RefCell<String>>) -> (gtk::Popover, Shown) {
    let resource = "shell/rooms.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        popover: gtk::Popover,
        pages: gtk::Stack,
        name: adw::EntryRow,
        create: gtk::Button,
        reach: adw::ActionRow,
        reach_change: gtk::Button,
        invite: adw::EntryRow,
        join: gtk::Button,
        invite_note: gtk::Label,
        failure: gtk::Label,
        room_group: adw::PreferencesGroup,
        room: adw::ActionRow,
        share: adw::SwitchRow,
        copy_invite: gtk::Button,
        copy_invite_icon: gtk::Image,
        room_reach: adw::ActionRow,
        use_relay: gtk::Button,
        leave: gtk::Button,
    });
    popover.connect_show(crate::widgets::interactions::align_topbar_popup);
    // It opens on Create, not on the name with its text selected.
    let (first, typed) = (create.downgrade(), name.downgrade());
    popover.connect_show(move |_| {
        if let (Some(create), Some(name)) = (first.upgrade(), typed.upgrade()) {
            create.grab_focus();
            name.select_region(-1, -1);
        }
    });
    let weak = Rc::downgrade(shell);
    share.connect_active_notify(move |switch| {
        let Some(shell) = weak.upgrade() else {
            return;
        };
        // What the room already says is its own news, not the listener's.
        let songs = switch.is_active();
        let sharing = (shell.products.room.status().room).map(|room| room.sharing);
        if sharing.is_some_and(|sharing| sharing != songs) {
            run(&shell, Action::Share { songs }, None);
        }
    });
    let (weak, called, failed) = (Rc::downgrade(shell), name.downgrade(), failure.downgrade());
    create.connect_clicked(move |_| {
        if let (Some(shell), Some(name)) = (weak.upgrade(), called.upgrade()) {
            let name = name.text().into();
            run(&shell, Action::Create { name }, failed.upgrade());
        }
    });
    let (weak, button, note) = (
        Rc::downgrade(shell),
        join.downgrade(),
        invite_note.downgrade(),
    );
    invite.connect_changed(move |invite| {
        let link = invite.text();
        // What the invite's room does with songs is read before joining.
        if let (Some(shell), Some(note)) = (weak.upgrade(), note.upgrade()) {
            if let Some(button) = button.upgrade() {
                let joining = shell.products.room.status().joining;
                button.set_sensitive(joining || !link.trim().is_empty());
            }
            let text = shell.products.room.invite_note(link.trim().into());
            note.set_visible(!text.is_empty());
            note.set_text(&text);
        }
    });
    let (weak, called, failed, link) = (
        Rc::downgrade(shell),
        name.downgrade(),
        failure.downgrade(),
        invite.downgrade(),
    );
    let joining = move || {
        if let (Some(shell), Some(name), Some(link)) =
            (weak.upgrade(), called.upgrade(), link.upgrade())
        {
            let action = match shell.products.room.status().joining {
                true => Action::Leave,
                false => Action::Join {
                    invite: link.text().trim().into(),
                    name: name.text().into(),
                },
            };
            run(&shell, action, failed.upgrade());
        }
    };
    let entered = joining.clone();
    invite.connect_entry_activated(move |_| entered());
    join.connect_clicked(move |_| joining());
    let (weak, popup) = (Rc::downgrade(shell), popover.downgrade());
    reach_change.connect_clicked(move |_| {
        if let Some(popover) = popup.upgrade() {
            popover.popdown();
        }
        if let Some(shell) = weak.upgrade() {
            (shell.preferences.integration).replace(Some("iroh_relay".into()));
            super::present_preferences_dialog_with_page(
                &shell,
                super::PreferencesPageKind::Integrations,
                false,
                false,
            );
        }
    });
    let weak = Rc::downgrade(shell);
    use_relay.connect_clicked(move |_| {
        if let Some(shell) = weak.upgrade() {
            run(&shell, Action::UseRelay, None);
        }
    });
    let copy = crate::widgets::clipboard::copy_action(copy_invite.upcast_ref(), &copy_invite_icon);
    let shared = shared.clone();
    copy_invite.connect_clicked(move |_| copy(&shared.borrow()));
    let (weak, popup) = (Rc::downgrade(shell), popover.downgrade());
    leave.connect_clicked(move |button| {
        let Some(shell) = weak.upgrade() else { return };
        let hosting = (shell.products.room.status().room).is_some_and(|room| room.hosting);
        if hosting {
            confirm_end(&shell, button);
        } else {
            run(&shell, Action::Leave, None);
        }
        if let Some(popover) = popup.upgrade() {
            popover.popdown();
        }
    });
    let shown = Shown {
        pages,
        name,
        create,
        reach,
        invite,
        join,
        failure,
        room_group,
        room,
        member_rows: RefCell::default(),
        share,
        room_reach,
        use_relay,
        leave,
    };
    (popover, shown)
}

/// Binds the Rooms button. Answers what an opened invite link does: it asks whether to join
/// the link's room.
pub fn bind_popover(shell: &Rc<Shell>, button: &gtk::MenuButton) -> impl Fn(&str) + 'static {
    let shown = Rc::new(RefCell::new(None::<Shown>));
    let offered = button.downgrade();
    let invite = Rc::new(RefCell::new(String::new()));
    let (weak, opened, link) = (Rc::downgrade(shell), shown.clone(), invite.clone());
    button.set_create_popup_func(move |button| {
        let Some(shell) = weak.upgrade() else {
            return;
        };
        if opened.borrow().is_none() {
            let (popover, parts) = popover(&shell, &link);
            button.set_popover(Some(&popover));
            opened.replace(Some(parts));
        }
        if let Some(parts) = opened.borrow().as_ref() {
            show(&shell, parts, &shell.products.room.status());
        }
    });
    let mut updates = shell.products.room.subscribe();
    let (weak, button) = (Rc::downgrade(shell), button.downgrade());
    gtk::glib::spawn_future_local(async move {
        let mut told = updates.borrow().notice.as_ref().map(|notice| notice.serial);
        let mut counted = None::<Option<usize>>;
        loop {
            let status = updates.borrow_and_update().clone();
            let (Some(shell), Some(button)) = (weak.upgrade(), button.upgrade()) else {
                break;
            };
            // Outside a room the button is its icon, as its neighbours; in one it also counts
            // who listens.
            let members = status.room.as_ref().map(|room| room.members.len());
            if counted.replace(members) != Some(members) {
                match members {
                    Some(members) => {
                        let content = adw::ButtonContent::builder()
                            .icon_name(ICON)
                            .label(members.to_string())
                            .build();
                        button.set_child(Some(&content));
                        button.add_css_class("counted");
                    }
                    None => {
                        button.set_icon_name(ICON);
                        button.remove_css_class("counted");
                    }
                }
            }
            let tooltip = match &status.room {
                Some(room) => {
                    button.add_css_class("accent");
                    room.name.clone()
                }
                None => {
                    button.remove_css_class("accent");
                    tr("Rooms")
                }
            };
            button.set_tooltip_text(Some(&tooltip));
            invite.replace(
                (status.room.as_ref())
                    .map(|room| room.invite.clone())
                    .unwrap_or_default(),
            );
            if let Some(parts) = shown.borrow().as_ref() {
                show(&shell, parts, &status);
            }
            if let Some(notice) = (status.notice.as_ref())
                .filter(|notice| told.replace(notice.serial) != Some(notice.serial))
            {
                (shell.control_feedback).show_feedback_toast(notice.text.clone());
            }
            drop(shell);
            if updates.changed().await.is_err() {
                break;
            }
        }
    });
    let (weak, button) = (Rc::downgrade(shell), offered);
    move |invite| {
        let (Some(shell), Some(button)) = (weak.upgrade(), button.upgrade()) else {
            return;
        };
        if shell.products.room.status().room.is_some() {
            (shell.control_feedback).show_feedback_toast(tr("Leave this room first"));
            return;
        }
        ask_join(&shell, &button, invite);
    }
}
