pub const CROSSFADE_DURATION_TITLE: &str = localization::msgid("Crossfade duration");
pub const PLAYBACK_SPEED_TITLE: &str = localization::msgid("Playback speed");
pub const PRESERVE_PITCH_TITLE: &str = localization::msgid("Preserve pitch");
use crate::widgets::scale::{install_scale_scroll_forwarding, install_sliding_value_bubble};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use localization::tr;
use playback::{
    CROSSFADE_MARKS, MAX_CROSSFADE_SECONDS, MAX_PLAYBACK_RATE, MIN_CROSSFADE_SECONDS,
    MIN_PLAYBACK_RATE, PLAYBACK_RATE_MARKS, PlaybackTransitionMode,
};

use crate::player::PlayerUi;

const PLAYBACK_RATE_COMMIT_DELAY: Duration = Duration::from_millis(200);
const PLAYBACK_SETTINGS_SCALE_WIDTH: i32 = 180;

pub fn configure_playback_settings_popover(
    button: &gtk::MenuButton,
    shell: &Rc<crate::shell::shell::Shell>,
) {
    let shell = Rc::downgrade(shell);
    // The popover opens on the page it was closed on, until the app exits.
    let on_equalizer = Rc::new(Cell::new(false));
    button.set_create_popup_func(move |button| {
        if let Some(shell) = shell.upgrade() {
            button.set_popover(Some(&playback_settings_popover(&shell, &on_equalizer)));
        }
    });
}

fn playback_settings_popover(
    window: &Rc<crate::shell::shell::Shell>,
    on_equalizer: &Rc<Cell<bool>>,
) -> gtk::Popover {
    let shell = &window.player_ui;
    let settings = window.settings.current.borrow().clone();
    let playback = settings.playback;
    let local_output = window.products.playback.playback_output().is_local();

    let resource = "player/playback_settings.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        popover: gtk::Popover,
        settings_group: adw::PreferencesGroup,
        queue: adw::SwitchRow,
        lyrics: adw::SwitchRow,
        visualizer: adw::SwitchRow,
        combine: adw::SwitchRow,
        audio_output: adw::ActionRow,
        pages: gtk::Stack,
        settings_page: gtk::ScrolledWindow,
        equalizer: adw::ActionRow,
        equalizer_state: gtk::Label,
        equalizer_page: gtk::Box,
        equalizer_back: gtk::Button,
        equalizer_host: gtk::Box,
        equalizer_header: gtk::CenterBox,
        equalizer_scroller: gtk::ScrolledWindow,
    });
    // The popover stays inside the window, above its button: each page scrolls in the height
    // between the button and the window's top, whatever room the compositor has outside it.
    let settings_scroller = settings_page.clone();
    let header_height = equalizer_page.spacing();
    let stack = pages.clone();
    popover.connect_show(move |popover| {
        let Some(button) = popover.parent() else {
            return;
        };
        let Some(bounds) = button.root().and_then(|root| button.compute_bounds(&root)) else {
            return;
        };
        let natural = |widget: &gtk::Widget| widget.measure(gtk::Orientation::Vertical, -1).1;
        let frame = natural(popover.upcast_ref()) - natural(stack.upcast_ref());
        let room = (bounds.y() as i32 - frame).max(1);
        settings_scroller.set_max_content_height(room);
        equalizer_scroller.set_max_content_height(
            (room - natural(equalizer_header.upcast_ref()) - header_height).max(1),
        );
    });
    let owner = window.products.playback.clone();
    let show_state = move |label: &gtk::Label| {
        let view = owner.equalizer_view(String::new());
        label.set_label(&if view.enabled {
            crate::player::equalizer::equalizer_preset_title(&view.name)
        } else {
            tr("Off")
        });
    };
    show_state(&equalizer_state);
    let weak = Rc::downgrade(window);
    let stack = pages.downgrade();
    let page = equalizer_page.downgrade();
    let remembered = Rc::clone(on_equalizer);
    let open_equalizer = move || {
        let (Some(window), Some(stack), Some(page)) =
            (weak.upgrade(), stack.upgrade(), page.upgrade())
        else {
            return;
        };
        if equalizer_host.first_child().is_none() {
            equalizer_host.append(&crate::player::equalizer::EqualizerSurface::new(&window).root);
        }
        stack.set_visible_child(&page);
        remembered.set(true);
    };
    if on_equalizer.get() {
        open_equalizer();
    }
    equalizer.connect_activated(move |_| open_equalizer());
    let stack = pages.downgrade();
    let remembered = Rc::clone(on_equalizer);
    equalizer_back.connect_clicked(move |_| {
        if let Some(stack) = stack.upgrade() {
            show_state(&equalizer_state);
            stack.set_visible_child(&settings_page);
            remembered.set(false);
        }
    });
    if !local_output {
        settings_group.set_description(Some(&tr("Audio processing is unavailable while casting")));
    }
    let device = crate::player::outputs::audio_output_dropdown(window, 180);
    crate::widgets::interactions::keep_parent_grab_for_dropdown(&device);
    audio_output.add_suffix(&device);
    audio_output.set_activatable_widget(Some(&device));
    audio_output.set_sensitive(local_output);
    let (transition_row, crossfade_options) =
        transition_rows(shell, &playback, PLAYBACK_SETTINGS_SCALE_WIDTH);
    transition_row.set_sensitive(local_output);
    crossfade_options.set_sensitive(local_output);
    settings_group.add(&transition_row);
    settings_group.add(&crossfade_options);
    let playback_rate =
        playback_rate_row(shell, playback.playback_rate, PLAYBACK_SETTINGS_SCALE_WIDTH);
    playback_rate.set_sensitive(local_output);
    settings_group.add(&playback_rate);
    let preserve_pitch = preserve_pitch_row(shell, playback.preserve_pitch);
    preserve_pitch.set_sensitive(local_output);
    settings_group.add(&preserve_pitch);

    queue.set_active(settings.right_panel.queue_visible);
    let queue_shell = Rc::downgrade(shell);
    queue.connect_active_notify(move |row| {
        if let Some(shell) = queue_shell.upgrade() {
            shell.set_sidebar_queue_visible(row.is_active());
        }
    });
    settings_group.add(&queue);

    lyrics.set_active(settings.lyrics_panel_visible);
    let lyrics_shell = Rc::clone(shell);
    lyrics.connect_active_notify(move |row| {
        lyrics_shell.show_media_panels(
            row.is_active(),
            lyrics_shell.right_panel.visualizer_visible.get(),
        );
    });
    settings_group.add(&lyrics);
    visualizer.set_active(settings.visualizer_panel_visible);
    visualizer.set_sensitive(local_output);
    let visualizer_shell = Rc::clone(shell);
    visualizer.connect_active_notify(move |row| {
        visualizer_shell
            .show_media_panels(visualizer_shell.lyrics.panel_visible.get(), row.is_active());
    });
    settings_group.add(&visualizer);
    combine.set_active(settings.right_panel.combined);
    let weak = Rc::downgrade(shell);
    combine.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.set_right_panel_combined(row.is_active());
        }
    });
    settings_group.add(&combine);
    popover
}

pub fn transition_rows(
    shell: &Rc<PlayerUi>,
    settings: &playback::PlaybackSettings,
    scale_width: i32,
) -> (adw::ActionRow, adw::PreferencesRow) {
    let resource = "player/transition_settings.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        mode: adw::ActionRow,
        crossfade: gtk::ToggleButton,
        options: adw::PreferencesRow,
        revealer: gtk::Revealer,
        rows: gtk::ListBox,
        skip_same_album: adw::SwitchRow,
    });
    rows.prepend(&crossfade_duration_row(
        shell,
        settings.crossfade_seconds,
        scale_width,
    ));
    let active = settings.transition_mode == PlaybackTransitionMode::Crossfade;
    crossfade.set_active(active);
    options.set_visible(active);
    revealer.set_reveal_child(active);
    skip_same_album.set_active(settings.skip_same_album_crossfade);
    let weak = Rc::downgrade(shell);
    skip_same_album.connect_active_notify(move |row| {
        if let Some(shell) = weak.upgrade() {
            shell.update_playback_settings(|settings| {
                settings.skip_same_album_crossfade = row.is_active()
            });
        }
    });
    let row = options.downgrade();
    revealer.connect_child_revealed_notify(move |revealer| {
        if !revealer.reveals_child()
            && !revealer.is_child_revealed()
            && let Some(row) = row.upgrade()
        {
            row.set_visible(false);
        }
    });
    let row = options.downgrade();
    let revealer = revealer.downgrade();
    let weak = Rc::downgrade(shell);
    crossfade.connect_active_notify(move |button| {
        let active = button.is_active();
        if let (Some(row), Some(revealer)) = (row.upgrade(), revealer.upgrade()) {
            if active {
                row.set_visible(true);
            }
            revealer.set_reveal_child(active);
        }
        if let Some(shell) = weak.upgrade() {
            shell.update_playback_settings(|settings| {
                settings.transition_mode = if active {
                    PlaybackTransitionMode::Crossfade
                } else {
                    PlaybackTransitionMode::Gapless
                };
            });
        }
    });
    (mode, options)
}

fn crossfade_duration_row(
    shell: &Rc<PlayerUi>,
    initial_seconds: u8,
    scale_width: i32,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(tr(CROSSFADE_DURATION_TITLE))
        .build();
    let scale = gtk::Scale::with_range(
        gtk::Orientation::Horizontal,
        f64::from(MIN_CROSSFADE_SECONDS),
        f64::from(MAX_CROSSFADE_SECONDS),
        1.0,
    );
    scale.add_css_class("playback-setting-scale");
    install_scale_scroll_forwarding(&scale);
    scale.set_width_request(scale_width);
    scale.set_valign(gtk::Align::Center);
    install_sliding_value_bubble(&scale, |seconds| format!("{seconds:.0}"));
    for mark in CROSSFADE_MARKS {
        scale.add_mark(mark, gtk::PositionType::Bottom, Some(&mark.to_string()));
    }
    scale.set_value(f64::from(initial_seconds));
    let crossfade_shell = Rc::clone(shell);
    scale.connect_value_changed(move |scale| {
        let seconds = scale.value().round() as u8;
        crossfade_shell.update_playback_settings(|settings| settings.crossfade_seconds = seconds);
    });
    row.add_suffix(&scale);
    row.set_activatable_widget(Some(&scale));
    row
}

pub fn playback_rate_row(
    shell: &Rc<PlayerUi>,
    initial_rate: f64,
    scale_width: i32,
) -> adw::ActionRow {
    let title_text = tr(PLAYBACK_SPEED_TITLE);
    let row = adw::ActionRow::builder().title(&title_text).build();
    let scale = gtk::Scale::with_range(
        gtk::Orientation::Horizontal,
        MIN_PLAYBACK_RATE,
        MAX_PLAYBACK_RATE,
        0.05,
    );
    scale.add_css_class("playback-setting-scale");
    install_scale_scroll_forwarding(&scale);
    scale.set_width_request(scale_width);
    install_sliding_value_bubble(&scale, playback_rate_value);
    for mark in PLAYBACK_RATE_MARKS {
        scale.add_mark(mark, gtk::PositionType::Bottom, Some(&mark.to_string()));
    }
    scale.set_value(initial_rate);
    scale.update_property(&[gtk::accessible::Property::Label(&title_text)]);
    row.add_suffix(&scale);
    row.set_activatable_widget(Some(&scale));

    let pending_rate = Rc::new(RefCell::new(None::<glib::SourceId>));
    let speed_shell = Rc::clone(shell);
    let pending_rate_for_change = Rc::clone(&pending_rate);
    scale.connect_value_changed(move |scale| {
        let rate = scale.value();
        if let Some(source) = pending_rate_for_change.borrow_mut().take() {
            source.remove();
        }
        let commit_shell = Rc::clone(&speed_shell);
        let commit_source = Rc::clone(&pending_rate_for_change);
        let source = glib::timeout_add_local_once(PLAYBACK_RATE_COMMIT_DELAY, move || {
            commit_source.borrow_mut().take();
            commit_shell.update_playback_settings(|settings| settings.playback_rate = rate);
        });
        pending_rate_for_change.borrow_mut().replace(source);
    });
    row
}

pub fn preserve_pitch_row(shell: &Rc<PlayerUi>, active: bool) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(tr(PRESERVE_PITCH_TITLE))
        .active(active)
        .build();
    let pitch_shell = Rc::clone(shell);
    row.connect_active_notify(move |row| {
        pitch_shell.update_playback_settings(|settings| {
            settings.preserve_pitch = row.is_active();
        });
    });
    row
}

fn playback_rate_value(rate: f64) -> String {
    let mut value = format!("{rate:.2}");
    while value.ends_with('0') {
        value.pop();
    }
    if value.ends_with('.') {
        value.push('0');
    }
    format!("{value} x")
}

#[cfg(test)]
mod tests {
    use super::playback_rate_value;

    #[test]
    fn playback_rate_value_keeps_a_clear_multiplier() {
        assert_eq!(playback_rate_value(0.5), "0.5 x");
        assert_eq!(playback_rate_value(1.0), "1.0 x");
        assert_eq!(playback_rate_value(1.25), "1.25 x");
        assert_eq!(playback_rate_value(2.0), "2.0 x");
    }
}

use playback::VolumeScale;
pub fn volume_scale_index(scale: VolumeScale) -> u32 {
    match scale {
        VolumeScale::Perceptual => 0,
        VolumeScale::Linear => 1,
    }
}
pub fn volume_scale_from_index(index: u32) -> VolumeScale {
    match index {
        1 => VolumeScale::Linear,
        _ => VolumeScale::Perceptual,
    }
}
