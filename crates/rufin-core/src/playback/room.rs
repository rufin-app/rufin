//! A room leads playback: this player's queue mirrors the room's, entry for entry, and its
//! playhead keeps to the room's timeline. What the listener does to the queue or the playhead
//! is asked of the room, which every member then follows.
use super::*;
use crate::playback::state::Command;
use crate::room::{Ask, Change, Entry, Modes, Session, Shared, Timeline};
use futures_util::future::BoxFuture;
use library::{QueueLoad, QueueRepeatMode, RoomSong, TrackMetadata, TrackReference};
use playback::{QueueProvenance, TransportStatus};
use rufin_connect::media::{PeerByteMetadata, PeerByteResponse};

/// How far this player may be off the room before it seeks.
const DRIFT_MILLIS: u64 = 2_000;

/// The queue row of a station the room plays: every member opens its stream itself, which
/// is a web address and nothing else.
fn station(song: &RoomSong) -> Option<String> {
    let stream = song.station.as_deref()?;
    (url::Url::parse(stream).ok())
        .filter(|address| matches!(address.scheme(), "http" | "https"))
        .map(|_| library::station_media_uri(stream))
}

/// What stands in this player's queue for a room's song: the listener's own copy, a station
/// by its stream, or the song by name where the library has none, which nothing can play.
fn reference(song: &RoomSong, copy: Option<&str>) -> TrackReference {
    if let Some(uri) = copy {
        return TrackReference {
            media_uri: uri.to_owned(),
            ..Default::default()
        };
    }
    TrackReference {
        media_uri: station(song).unwrap_or_else(|| crate::room::lacking_uri(song)),
        metadata: TrackMetadata {
            title: song.title.clone(),
            artist: song.artist.clone(),
            album: song.album.clone(),
            duration_millis: song.duration_millis,
            ..Default::default()
        },
        artwork_binding: None,
    }
}

/// What a room is asked for songs a play places: they become its queue, playing the one at
/// `start`, or go after its current song or at its end.
fn placed(room: &Session, placement: QueuePlacement, songs: Vec<RoomSong>, start: usize) -> Ask {
    let after = match placement {
        QueuePlacement::Replace { .. } => return Ask::Replace { songs, start },
        QueuePlacement::End => room.read(|shared| shared.entries.last().map(|entry| entry.id)),
        QueuePlacement::AfterCurrent => room.read(|shared| shared.timeline.current),
    };
    Ask::Add { songs, after }
}

/// The entry the room places things after for a target in this player's queue, given the
/// positions that move: the nearest entry before the target that stays.
fn after(shared: &Shared, target: &QueueReorderTarget, moving: &[usize]) -> Option<u64> {
    let before = match *target {
        QueueReorderTarget::Before(position) => position,
        QueueReorderTarget::After(position) => position + 1,
        _ => shared.entries.len(),
    };
    (0..before.min(shared.entries.len()))
        .rev()
        .find(|position| !moving.contains(position))
        .map(|position| shared.entries[position].id)
}

/// A room's song as the member who has it streams it.
struct MemberAudio(std::sync::Weak<Session>);

impl sources::PeerLink for MemberAudio {
    fn open<'a>(
        &'a self,
        _device: &'a str,
        _source: &'a sources::SourceId,
        object: &'a str,
        encoding: Option<library::AudioEncoding>,
        _range: Option<(u64, Option<u64>)>,
        window: Option<(u64, Option<u64>)>,
    ) -> BoxFuture<'a, sources::SourceResult<sources::PeerAudio>> {
        Box::pin(async move {
            let absent = || sources::SourceError::NotFound;
            let session = self.0.upgrade().ok_or_else(absent)?;
            let entry = object.parse().map_err(|_| absent())?;
            let audio = (session
                .audio(entry, encoding.ok_or_else(absent)?, window)
                .await)
                .map_err(sources::SourceError::Network)?;
            let about: PeerByteMetadata = serde_json::from_value(audio.metadata)?;
            Ok(sources::PeerAudio {
                reader: audio.reader,
                length: about.length,
                offset: about.offset,
                content_type: about.content_type,
                validator: about.validator,
            })
        })
    }

    // A room has no catalog to read: its songs are named by the room itself.
    fn marker<'a>(
        &'a self,
        _: &'a str,
        _: &'a sources::SourceId,
    ) -> BoxFuture<'a, sources::SourceResult<library::Freshness>> {
        Box::pin(std::future::ready(Err(sources::SourceError::NotFound)))
    }
    fn ids<'a>(
        &'a self,
        _: &'a str,
        _: &'a sources::SourceId,
    ) -> BoxFuture<'a, sources::SourceResult<(Vec<String>, i64)>> {
        Box::pin(std::future::ready(Err(sources::SourceError::NotFound)))
    }
    fn changed_since<'a>(
        &'a self,
        _: &'a str,
        _: &'a sources::SourceId,
        _: i64,
    ) -> BoxFuture<'a, sources::SourceResult<Vec<String>>> {
        Box::pin(std::future::ready(Err(sources::SourceError::NotFound)))
    }
    fn observations<'a>(
        &'a self,
        _: &'a str,
        _: &'a sources::SourceId,
        _: &'a [String],
    ) -> BoxFuture<'a, sources::SourceResult<Vec<library::PeerTrack>>> {
        Box::pin(std::future::ready(Err(sources::SourceError::NotFound)))
    }
    fn watch<'a>(
        &'a self,
        _: &'a str,
        _: &'a sources::SourceId,
    ) -> BoxFuture<
        'a,
        sources::SourceResult<std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>>>,
    > {
        Box::pin(std::future::ready(Err(sources::SourceError::NotFound)))
    }
    fn image<'a>(
        &'a self,
        _: &'a str,
        _: &'a sources::SourceId,
        _: &'a str,
        _: Option<u32>,
    ) -> BoxFuture<'a, sources::SourceResult<sources::ImageBytes>> {
        Box::pin(std::future::ready(Err(sources::SourceError::NotFound)))
    }
}

/// How a room's song this library lacks plays: as a stream from a member who has it, where
/// the room shares songs. What crosses is a stream encoded for the listener, never the
/// member's file; a listener who plays originals gets it lossless. It is held in `kept`
/// while it plays, sealed, and nowhere after.
pub(super) async fn lacking_stream(
    room: Option<Arc<Session>>,
    uri: &str,
    quality: library::AudioQuality,
    kept: Option<std::path::PathBuf>,
) -> Result<audio_processing::ResolvedStream, playback::BackendFailure> {
    let unavailable = || {
        let kind = playback::FailureKind::UnavailableMedia;
        playback::BackendFailure::with_kind(kind, kind.summary())
    };
    let session = room.ok_or_else(unavailable)?;
    let entry = session.lacking(uri).ok_or_else(unavailable)?;
    let encoding = quality.encoding().unwrap_or(library::AudioEncoding {
        codec: library::AudioCodec::Flac,
        bitrate_kbps: None,
        channels: Default::default(),
    });
    let link = Arc::new(MemberAudio(Arc::downgrade(&session)));
    sources::PeerSource::over("room".into(), sources::SourceId::new("rufin-room"), link)
        .object_stream(&entry.to_string(), uri, None, Some(encoding), kept)
        .await
        .map_err(|error| playback::BackendFailure::new(error.to_string()))
}

impl PlaybackOwner {
    /// This device's copy of a room's song from `window` on, encoded as the member who
    /// asks for it plays it.
    pub(crate) async fn room_audio(
        &self,
        song: &RoomSong,
        encoding: library::AudioEncoding,
        window: Option<(u64, Option<u64>)>,
    ) -> Result<PeerByteResponse, String> {
        let absent = || localization::tr("This song is not on this device");
        let own = self.own_songs().await?;
        let copy = {
            let mine = self.room_mine.lock().unwrap_or_else(|p| p.into_inner());
            (mine.get(song).map(String::as_str))
                .or_else(|| own.copy(song))
                .map(str::to_owned)
        }
        .ok_or_else(absent)?;
        let playable = (self.catalog.playable_by_uri(&copy).await)
            .map_err(string_error)?
            .ok_or_else(absent)?;
        let local = self.settings.local_configuration();
        let source_owner = Arc::clone(&self.source);
        let downloads = source_owner.downloads().clone();
        let stream = prepare_stream(
            &playable,
            StreamRequest::original(copy),
            local.as_ref(),
            Some(&downloads),
            move |source_id| async move {
                tokio::task::spawn_blocking(move || source_owner.client(&source_id))
                    .await
                    .map_err(string_error)?
            },
        )
        .await
        .map_err(|failure| failure.to_string())?;
        // A track cut out of a longer file is asked for from its own start.
        let window = match (playable.window, window) {
            (Some((start, end)), Some((from, to))) => Some((
                start + from,
                Some(to.map_or(end, |to| (start + to).min(end))),
            )),
            (Some((start, end)), None) => Some((start, Some(end))),
            (None, window) => window,
        };
        crate::connect::peer::transcoded(stream, encoding, window).await
    }

    /// Whether another device plays for this one.
    pub(crate) fn casting(&self) -> bool {
        *self.output.lock().unwrap_or_else(|p| p.into_inner()) != playback::PlaybackOutput::Local
    }

    pub(super) fn room(&self) -> Option<Arc<Session>> {
        self.room.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// A command for this player itself, past the room.
    pub(super) fn own(&self, command: Command) {
        if let Some(active) = self.active() {
            active.downgrade().post(move |player, sample| {
                if let Err(error) = player.handle_command(command, sample) {
                    player.report_error(error.to_string());
                }
            });
        }
    }

    /// What stands for each server this library lists: its kind and address, which every
    /// device that lists the server has alike.
    fn servers(&self) -> library::RoomServers {
        (self.settings.load().sources.configured.iter())
            .filter_map(|configured| {
                let source = &configured.configuration;
                let address = source.server_address()?;
                Some((
                    source.source_id.to_string(),
                    format!("{}\n{}", source.kind, address.trim_end_matches('/')),
                ))
            })
            .collect()
    }

    /// Songs this device gives the room, as the room will hold them. The tracks they come
    /// from are this device's copies of them, whatever else its library holds.
    fn mine(&self, songs: impl IntoIterator<Item = (RoomSong, String)>) -> Vec<RoomSong> {
        let mut mine = self.room_mine.lock().unwrap_or_else(|p| p.into_inner());
        songs
            .into_iter()
            .map(|(song, uri)| {
                let song = crate::room::clipped(song);
                mine.insert(song.clone(), uri);
                song
            })
            .collect()
    }

    /// This player takes the room's modes: it shows them, repeats as the room does and keeps
    /// the room's order as its own. Only the host's Auto DJ fills the queue.
    fn room_modes(&self, modes: Modes) {
        self.own(Command::Follow(Some(modes)));
        self.own(Command::SetRepeat(modes.repeat));
        self.own(Command::SetShuffle {
            enabled: false,
            seed: 0,
        });
        self.own(Command::SetAutoDj {
            enabled: modes.auto_dj && self.room().is_some_and(|room| room.hosting),
            refill_threshold: usize::from(
                self.settings.load().ui.application.auto_dj_refill_threshold,
            ),
        });
    }

    /// This library's tracks as the room will hold them, given by this device.
    pub(super) async fn room_gives(
        &self,
        keys: &[library::TrackKey],
    ) -> Result<Vec<RoomSong>, String> {
        let songs = (self.catalog.room_songs(keys, &self.servers()).await).map_err(string_error)?;
        Ok(self.mine(songs.into_iter().flatten()))
    }

    /// What stands in this player's queue for the room's `entries`, and whether this device
    /// can play each by itself.
    async fn queue_of(&self, entries: &[Entry]) -> Result<(QueueLoad, Vec<bool>), String> {
        let own = self.own_songs().await?;
        let copies = {
            let mine = self.room_mine.lock().unwrap_or_else(|p| p.into_inner());
            entries
                .iter()
                .map(|entry| {
                    (mine.get(&entry.song).map(String::as_str))
                        .or_else(|| own.copy(&entry.song))
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>()
        };
        let items = entries
            .iter()
            .zip(&copies)
            .map(|(entry, copy)| {
                (
                    reference(&entry.song, copy.as_deref()),
                    QueueProvenance::Manual,
                )
            })
            .collect();
        let queued = self
            .catalog
            .queue_entries(QueueLoad::items(items), None)
            .await
            .map_err(string_error)?
            .entries;
        let plays = entries
            .iter()
            .zip(&copies)
            .map(|(entry, copy)| copy.is_some() || station(&entry.song).is_some())
            .collect();
        Ok((QueueLoad::Captured(queued.into()), plays))
    }

    /// This player's positions for a selection of its queue.
    fn positions(&self, selection: &QueueSelection) -> Vec<usize> {
        match selection {
            QueueSelection::Positions(positions) => positions.clone(),
            QueueSelection::Occurrences(ids) => {
                let Some(Ok((entries, _))) = self
                    .active()
                    .map(|active| active.page_entries(0, usize::MAX))
                else {
                    return Vec::new();
                };
                entries
                    .into_iter()
                    .filter(|(_, id, _)| id.is_some_and(|id| ids.contains(&id)))
                    .map(|(position, ..)| position)
                    .collect()
            }
        }
    }

    /// This device hosts a room of its own queue: the songs it holds in order, the one it
    /// plays, where in it and whether it plays. The queue stays as it plays.
    pub(crate) async fn host_room(
        &self,
        room: Arc<Session>,
    ) -> Result<(Vec<RoomSong>, Option<usize>, u64, bool), String> {
        let active = self.active().ok_or("Playback is unavailable")?;
        *self.room.lock().unwrap_or_else(|p| p.into_inner()) = Some(room);
        self.room_modes(Modes::default());
        let (entries, _) = tokio::task::spawn_blocking(move || active.page_entries(0, usize::MAX))
            .await
            .map_err(string_error)?
            .map_err(string_error)?;
        let keys = entries
            .iter()
            .map(|(.., entry)| entry.track_key)
            .collect::<Vec<_>>();
        let songs =
            (self.catalog.room_songs(&keys, &self.servers()).await).map_err(string_error)?;
        // A track the Store no longer holds leaves this queue: the room cannot name it.
        let gone = songs
            .iter()
            .enumerate()
            .filter(|(_, song)| song.is_none())
            .map(|(position, _)| position)
            .collect::<Vec<_>>();
        if !gone.is_empty() {
            self.own(Command::RemoveMany(QueueSelection::Positions(gone.clone())));
        }
        let mut songs = self.mine(songs.into_iter().flatten());
        // A room holds so many songs; the rest leave this queue.
        if songs.len() > crate::room::ENTRIES {
            self.own(Command::RemoveMany(QueueSelection::Positions(
                (crate::room::ENTRIES..songs.len()).collect(),
            )));
            songs.truncate(crate::room::ENTRIES);
        }
        let here = self.observed(self.current_projection().as_ref());
        let current = here
            .current
            .filter(|position| !gone.contains(position))
            .map(|position| position - gone.iter().filter(|gone| **gone < position).count())
            .filter(|position| *position < songs.len());
        Ok((
            songs,
            current,
            here.position_ms,
            here.playing && current.is_some(),
        ))
    }

    /// This device joins a room: its own queue stays saved and leaves the player.
    pub(crate) fn join_room(&self, room: Arc<Session>) {
        *self.room.lock().unwrap_or_else(|p| p.into_inner()) = Some(room);
        self.own(Command::Clear {
            include_current: true,
        });
        self.room_modes(Modes::default());
    }

    /// The room is over for this device: its saved queue comes back, paused.
    pub(crate) async fn leave_room(&self) {
        if self
            .room
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .is_none()
        {
            return;
        }
        self.room_songs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        self.room_mine
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        self.own(Command::Pause);
        self.own(Command::Follow(None));
        let saved = self.catalog.restore_queue(false).await.unwrap_or_default();
        let stored = self.settings.load();
        let entries = saved
            .order
            .iter()
            .filter_map(|member| saved.entries.get(*member as usize).cloned())
            .collect::<Vec<_>>();
        let (repeat, shuffled) = (saved.repeat_mode, saved.shuffled);
        let resumed =
            saved.current_index.is_some() && self.continue_queue(saved, false, None).await.is_ok();
        if !resumed {
            self.own(Command::Clear {
                include_current: true,
            });
            if !entries.is_empty() {
                self.own(Command::Insert {
                    input: QueueLoad::Captured(entries.into()),
                    target: QueueReorderTarget::End,
                });
            }
            self.own(Command::SetRepeat(repeat));
            if shuffled {
                self.own(Command::SetShuffle {
                    enabled: true,
                    seed: random_u64(),
                });
            }
        }
        self.own(Command::SetAutoDj {
            enabled: stored.ui.auto_dj_enabled,
            refill_threshold: usize::from(stored.ui.application.auto_dj_refill_threshold),
        });
    }

    /// Makes this player's queue follow one change of the room's, given the room before it.
    /// Answers the added songs this library has, and the ones it lacks.
    pub(crate) async fn room_changed(
        &self,
        before: &Shared,
        change: &Change,
    ) -> Result<(Vec<u64>, Vec<u64>), String> {
        let positions = |ids: &[u64]| {
            ids.iter()
                .filter_map(|id| before.index(*id))
                .collect::<Vec<_>>()
        };
        match change {
            Change::Added { entries, after } => {
                let (queued, plays) = self.queue_of(entries).await?;
                let target = match after.map(|id| before.index(id)) {
                    None if !before.entries.is_empty() => QueueReorderTarget::Before(0),
                    Some(Some(position)) => QueueReorderTarget::After(position),
                    _ => QueueReorderTarget::End,
                };
                self.own(Command::Insert {
                    input: queued,
                    target,
                });
                let total = before.entries.len() + entries.len();
                self.queue_holds(|held| held >= total).await;
                let (has, lacks): (Vec<_>, Vec<_>) =
                    entries.iter().zip(plays).partition(|(_, plays)| *plays);
                let ids = |entries: Vec<(&Entry, bool)>| {
                    entries.into_iter().map(|(entry, _)| entry.id).collect()
                };
                return Ok((ids(has), ids(lacks)));
            }
            Change::Removed { ids } => {
                let positions = positions(ids);
                if !positions.is_empty() {
                    self.own(Command::RemoveMany(QueueSelection::Positions(positions)));
                }
            }
            Change::Moved { ids, after } => {
                let moving = positions(ids);
                let target = match after {
                    Some(id) => before.index(*id).map(QueueReorderTarget::After),
                    None => (0..before.entries.len())
                        .find(|position| !moving.contains(position))
                        .map(QueueReorderTarget::Before),
                };
                if let Some(target) = target.filter(|_| !moving.is_empty()) {
                    self.own(Command::Reorder {
                        selection: QueueSelection::Positions(moving),
                        target,
                    });
                }
            }
            Change::Cleared => {
                self.own(Command::Clear {
                    include_current: true,
                });
                // What the room adds next goes into the emptied queue.
                self.queue_holds(|held| held == 0).await;
            }
            Change::Ordered { .. } => {
                let mut after = before.clone();
                after.apply(change);
                let total = before.entries.len();
                let keep = (before.entries.iter().zip(&after.entries))
                    .take_while(|(before, after)| before.id == after.id)
                    .count();
                // The song that plays stays in the player; the rest leaves and comes back
                // around it in the room's order.
                let current = (before.timeline.current)
                    .and_then(|id| Some((id, before.index(id)?)))
                    .filter(|(_, position)| *position >= keep);
                let leaving = (keep..total)
                    .filter(|position| current.is_none_or(|(_, current)| current != *position))
                    .collect::<Vec<_>>();
                if leaving.is_empty() {
                    return Ok((Vec::new(), Vec::new()));
                }
                self.own(Command::RemoveMany(QueueSelection::Positions(leaving)));
                let kept = keep + usize::from(current.is_some());
                self.queue_holds(|held| held == kept).await;
                let at = current.and_then(|(id, _)| after.index(id));
                let (ahead, behind) = match at {
                    Some(at) => (&after.entries[keep..at], &after.entries[at + 1..]),
                    None => (&after.entries[..0], &after.entries[keep..]),
                };
                for (entries, target, held) in [
                    (ahead, QueueReorderTarget::Before(keep), kept + ahead.len()),
                    (behind, QueueReorderTarget::End, total),
                ] {
                    if !entries.is_empty() {
                        let (input, _) = self.queue_of(entries).await?;
                        self.own(Command::Insert { input, target });
                        self.queue_holds(|now| now >= held).await;
                    }
                }
            }
            Change::Modes(modes) => self.room_modes(*modes),
            Change::Timeline(_) => {}
        }
        Ok((Vec::new(), Vec::new()))
    }

    /// The songs this library has, read once for the room this player follows.
    async fn own_songs(&self) -> Result<Arc<library::OwnSongs>, String> {
        let known = self
            .room_songs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(known) = known {
            return Ok(known);
        }
        let own = (self.catalog.own_songs(&self.servers()).await).map_err(string_error)?;
        let own = Arc::new(own);
        *self.room_songs.lock().unwrap_or_else(|p| p.into_inner()) = Some(own.clone());
        Ok(own)
    }

    /// Waits until the player's queue holds what the room's last change leaves in it: it
    /// reads the rows of the entries it is given before it places them.
    pub(super) async fn queue_holds(&self, enough: impl Fn(usize) -> bool) {
        for _ in 0..250 {
            let held = self
                .current_projection()
                .map_or(0, |projection| projection.view.queue.total);
            if enough(held) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Brings this player to the room's song and place at the room's time `now`. `before` is
    /// the room's timeline before a change of it, which also says whether this player plays;
    /// between changes only a playing player is put right. A device that paused for itself
    /// while the room played (a call, a sleep timer) stays paused through the room's changes
    /// until the room itself is resumed or the listener plays.
    pub(crate) fn follow_room(&self, shared: &Shared, now: u64, before: Option<Timeline>) {
        let here = self.observed(self.current_projection().as_ref());
        let timeline = shared.timeline;
        let sounding = matches!(
            here.state,
            TransportStatus::Playing | TransportStatus::Buffering | TransportStatus::Resolving
        );
        let Some(position) = timeline.current.and_then(|id| shared.index(id)) else {
            if before.is_some() && here.playing {
                self.own(Command::Pause);
            }
            return;
        };
        if !(before.is_some() || here.playing && sounding) {
            return;
        }
        let own_pause =
            before.is_some_and(|before| before.playing) && here.state == TransportStatus::Paused;
        let song = &shared.entries[position].song;
        // A station is live on every device: there is no place in it to keep.
        let live = song.station.is_some();
        let wanted = timeline.position(now);
        // This copy ended a moment before the room's song does: the player is already where
        // the room goes next.
        let next = match shared.modes.repeat {
            QueueRepeatMode::One => position,
            QueueRepeatMode::All if position + 1 == shared.entries.len() => 0,
            _ => position + 1,
        };
        let early = wanted + DRIFT_MILLIS >= song.duration_millis.max(0) as u64
            && here.current == Some(next)
            && (next != position || here.position_ms < DRIFT_MILLIS);
        if before.is_none() && early && !live {
            return;
        }
        if here.current != Some(position) {
            self.own(Command::Activate(position));
            if wanted > DRIFT_MILLIS && !live {
                self.own(Command::Seek(wanted));
            }
            if !timeline.playing || own_pause {
                self.own(Command::Pause);
            }
            return;
        }
        if here.position_ms.abs_diff(wanted) > DRIFT_MILLIS && !live {
            self.own(Command::Seek(wanted));
        }
        match before {
            Some(_) if !timeline.playing && here.playing => self.own(Command::Pause),
            // The room was resumed, or its queue was put anew under this song: everyone
            // plays again.
            Some(_) if timeline.playing && !here.playing && !own_pause => self.own(Command::Play),
            _ => {}
        }
    }

    /// Asks the room for what a play does: its songs become the room's queue and the room
    /// plays the one the play starts at; a play that adds puts them after the room's
    /// current song or at the end.
    pub(super) fn play_room(&self, request: &PlayRequest) -> bool {
        let Some(room) = self.room() else {
            return false;
        };
        let request = request.clone();
        let catalog = self.catalog.clone();
        let servers = self.servers();
        let this = self.this.clone();
        self.runtime.spawn(async move {
            let asked = async {
                let placement = request.placement;
                // A room that shuffles starts a play where a shuffled player would.
                let shuffled = room.read(|shared| shared.modes.shuffle);
                let page = request.prepare(&catalog, shuffled, random_u64()).await?;
                let replaces = matches!(placement, QueuePlacement::Replace { .. });
                // A room takes so many songs at once: a longer play starts at its song.
                let from = match page.entries.len() > crate::room::ADDED_AT_ONCE {
                    true if replaces => page.current_index,
                    _ => 0,
                };
                let keys = page.entries[from..]
                    .iter()
                    .take(crate::room::ADDED_AT_ONCE)
                    .map(|entry| entry.track_key)
                    .collect::<Vec<_>>();
                let songs = (catalog.room_songs(&keys, &servers).await).map_err(string_error)?;
                let Some(owner) = this.upgrade() else {
                    return Ok(());
                };
                // A track the Store no longer holds is not in the room: the play starts at
                // the song that stands where it was.
                let start = songs[..page.current_index - from]
                    .iter()
                    .filter(|song| song.is_some())
                    .count();
                let songs = owner.mine(songs.into_iter().flatten());
                room.ask(placed(&room, placement, songs, start));
                Ok::<_, String>(())
            };
            if let (Err(error), Some(owner)) = (asked.await, this.upgrade()) {
                owner.own(Command::OperationFailed(error));
            }
        });
        true
    }

    /// What the listener does to the queue or the playhead becomes a request to the room.
    /// A command that is this device's own business comes back.
    pub(super) fn send_room(&self, command: Command) -> Option<Command> {
        let Some(room) = self.room() else {
            return Some(command);
        };
        let here = self.observed(self.current_projection().as_ref());
        let (current, playing) =
            room.read(|shared| (shared.timeline.current, shared.timeline.playing));
        let id_at =
            |position: usize| room.read(|shared| shared.entries.get(position).map(|e| e.id));
        let ids = |positions: &[usize]| {
            room.read(|shared| {
                positions
                    .iter()
                    .filter_map(|position| shared.entries.get(*position).map(|e| e.id))
                    .collect::<Vec<_>>()
            })
        };
        // The one button plays or pauses by what this device does, as outside a room.
        let command = match command {
            Command::PlayPause if here.playing => Command::Pause,
            Command::PlayPause => Command::Play,
            command => command,
        };
        let ask = match command {
            // A device that paused for itself catches up with a room that plays.
            Command::Play if playing && !here.playing => {
                self.own(Command::Play);
                return None;
            }
            Command::Play => Some(Ask::Play),
            Command::Pause | Command::Stop => Some(Ask::Pause),
            Command::Next => current.map(|from| Ask::Skip { from }),
            Command::Previous => current.map(|from| Ask::Back { from }),
            Command::Seek(millis) => current.map(|id| Ask::Seek { id, millis }),
            Command::Activate(position) => id_at(position).map(|id| Ask::Jump { id }),
            Command::ActivateOccurrence(id) => self
                .positions(&QueueSelection::Occurrences(vec![id]))
                .first()
                .and_then(|position| id_at(*position))
                .map(|id| Ask::Jump { id }),
            Command::Remove(position) => Some(Ask::Remove {
                ids: ids(&[position]),
            }),
            Command::RemoveMany(selection) => Some(Ask::Remove {
                ids: ids(&self.positions(&selection)),
            }),
            Command::Reorder { selection, target } => {
                let moving = self.positions(&selection);
                let target = self.target(target);
                Some(Ask::Move {
                    ids: ids(&moving),
                    after: room.read(|shared| after(shared, &target, &moving)),
                })
            }
            Command::MoveAfterCurrentSelection(selection) => Some(Ask::Move {
                ids: ids(&self.positions(&selection)),
                after: current,
            }),
            Command::Insert { input, target } => {
                let target = self.target(target);
                let catalog = self.catalog.clone();
                let this = self.this.clone();
                self.runtime.spawn(async move {
                    let Ok(queued) = catalog.queue_entries(input, None).await else {
                        return;
                    };
                    let keys = queued
                        .entries
                        .iter()
                        .map(|entry| entry.track_key)
                        .collect::<Vec<_>>();
                    let Some(owner) = this.upgrade() else {
                        return;
                    };
                    if let Ok(songs) = owner.room_gives(&keys).await {
                        room.ask(Ask::Add {
                            songs,
                            after: room.read(|shared| after(shared, &target, &[])),
                        });
                    }
                });
                return None;
            }
            Command::Clear { .. } => Some(Ask::Clear),
            Command::SetRepeat(mode) => Some(Ask::Repeat { mode }),
            Command::SetShuffle { enabled, seed } => Some(Ask::Shuffle { on: enabled, seed }),
            // The room's Auto DJ is turned on and off as the room's; the host's player takes
            // how early it fills the queue.
            Command::SetAutoDj {
                refill_threshold, ..
            } => {
                if room.hosting {
                    self.own(Command::SetAutoDj {
                        enabled: room.read(|shared| shared.modes.auto_dj),
                        refill_threshold,
                    });
                }
                return None;
            }
            // Songs like the one the radio starts from, out of this library, played as any
            // play is.
            Command::Radio(request) => {
                let candidates = self.candidates(request.seed, 20, Default::default());
                let this = self.this.clone();
                self.runtime.spawn(async move {
                    let Some(owner) = this.upgrade() else {
                        return;
                    };
                    let songs = async { owner.room_gives(&candidates.await?).await };
                    match songs.await {
                        Ok(songs) => room.ask(placed(&room, request.placement, songs, 0)),
                        Err(error) => owner.own(Command::OperationFailed(error)),
                    }
                });
                return None;
            }
            Command::ApplyQueue(_) => {
                self.own(Command::OperationFailed(localization::tr(
                    "Not available while listening together",
                )));
                return None;
            }
            command => return Some(command),
        };
        if let Some(ask) = ask {
            room.ask(ask);
        }
        None
    }

    /// A target naming an occurrence of this player's queue, as a position.
    fn target(&self, target: QueueReorderTarget) -> QueueReorderTarget {
        let position = |id: u64| {
            self.positions(&QueueSelection::Occurrences(vec![id]))
                .first()
                .copied()
        };
        match target {
            QueueReorderTarget::BeforeOccurrence(id) => {
                position(id).map_or(QueueReorderTarget::End, QueueReorderTarget::Before)
            }
            QueueReorderTarget::AfterOccurrence(id) => {
                position(id).map_or(QueueReorderTarget::End, QueueReorderTarget::After)
            }
            target => target,
        }
    }
}
