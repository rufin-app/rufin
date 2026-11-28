//! A room's queue and timeline, the changes the host makes to them and what members ask for.
//! The host decides; every member applies the host's changes in the order they come.
use std::collections::{HashMap, HashSet};

use library::{QueueRepeatMode, RoomSong};
use serde::{Deserialize, Serialize};

pub(super) const VERSION: u32 = 1;
pub(super) const MEMBERS: usize = 16;
pub(crate) const ENTRIES: usize = 3000;
pub(crate) const ADDED_AT_ONCE: usize = 500;
const TEXT: usize = 300;
const ADDRESS: usize = 2000;
/// A song nobody has is passed over this long after its turn came, once members had the time
/// to say whether they have it.
const UNHEARD_MILLIS: u64 = 3_000;
/// Going back restarts a song that played this long; before that it goes to the one before.
const RESTART_MILLIS: u64 = 3_000;
pub(super) const UNDO_MILLIS: u64 = 15_000;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub id: u64,
    pub song: RoomSong,
    /// The member that added it.
    pub by: String,
}

/// What a member asks of the room. Each names what it acts on, so two members asking the
/// same thing at once change it once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case")]
pub(crate) enum Ask {
    /// Songs after the entry `after`, or first without one.
    Add {
        songs: Vec<RoomSong>,
        after: Option<u64>,
    },
    /// The room's queue becomes these songs and the room plays the one at `start`.
    Replace {
        songs: Vec<RoomSong>,
        start: usize,
    },
    Remove {
        ids: Vec<u64>,
    },
    Move {
        ids: Vec<u64>,
        after: Option<u64>,
    },
    Jump {
        id: u64,
    },
    Play,
    Pause,
    Seek {
        id: u64,
        millis: u64,
    },
    Skip {
        from: u64,
    },
    Back {
        from: u64,
    },
    Clear,
    /// Takes back the asker's `Replace`: the queue it replaced comes back where it was.
    Undo,
    Repeat {
        mode: QueueRepeatMode,
    },
    /// Shuffles the songs after the current one, or puts the queue back in the order it
    /// would have without.
    Shuffle {
        on: bool,
        seed: u64,
    },
    /// The host's Auto DJ adds songs when the room's queue runs low.
    AutoDj {
        on: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub(crate) enum Change {
    Added {
        entries: Vec<Entry>,
        after: Option<u64>,
    },
    Removed {
        ids: Vec<u64>,
    },
    Moved {
        ids: Vec<u64>,
        after: Option<u64>,
    },
    Cleared,
    Timeline(Timeline),
    /// The queue's whole order.
    Ordered {
        ids: Vec<u64>,
    },
    Modes(Modes),
}

/// How the room plays its queue, for every member alike.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Modes {
    #[serde(default)]
    pub repeat: QueueRepeatMode,
    #[serde(default)]
    pub shuffle: bool,
    /// The host's Auto DJ fills the queue.
    #[serde(default)]
    pub auto_dj: bool,
}

/// Where the room is in its current song: `millis` into it at the room's clock time `at`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Timeline {
    pub current: Option<u64>,
    pub millis: u64,
    pub at: u64,
    pub playing: bool,
}

impl Timeline {
    pub(crate) fn position(&self, now: u64) -> u64 {
        if self.playing {
            self.millis + now.saturating_sub(self.at)
        } else {
            self.millis
        }
    }
}

/// The room as every member holds it.
#[derive(Clone, Debug, Default)]
pub(crate) struct Shared {
    pub entries: Vec<Entry>,
    pub timeline: Timeline,
    pub modes: Modes,
}

impl Shared {
    pub(crate) fn index(&self, id: u64) -> Option<usize> {
        self.entries.iter().position(|entry| entry.id == id)
    }

    /// Where entries placed after `after` go: behind it, first without one, last when it left.
    fn place(&self, after: Option<u64>) -> usize {
        match after {
            None => 0,
            Some(id) => self.index(id).map_or(self.entries.len(), |index| index + 1),
        }
    }

    pub(crate) fn apply(&mut self, change: &Change) {
        match change {
            Change::Added { entries, after } => {
                let place = self.place(*after);
                self.entries.splice(place..place, entries.iter().cloned());
            }
            Change::Removed { ids } => self.entries.retain(|entry| !ids.contains(&entry.id)),
            Change::Moved { ids, after } => {
                if after.is_some_and(|id| ids.contains(&id)) {
                    return;
                }
                // The moved entries keep the order they stand in, as the player moves them.
                let (moved, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.entries)
                    .into_iter()
                    .partition(|entry| ids.contains(&entry.id));
                self.entries = kept;
                let place = self.place(*after);
                self.entries.splice(place..place, moved);
            }
            Change::Cleared => {
                self.entries.clear();
                self.timeline = Timeline::default();
            }
            Change::Timeline(timeline) => self.timeline = *timeline,
            Change::Ordered { ids } => {
                // An entry the order does not name stays behind the ones it names.
                let places = (ids.iter().enumerate())
                    .map(|(place, id)| (*id, place))
                    .collect::<HashMap<_, _>>();
                (self.entries).sort_by_cached_key(|entry| {
                    places.get(&entry.id).copied().unwrap_or(ids.len())
                });
            }
            Change::Modes(modes) => self.modes = *modes,
        }
    }
}

struct Undo {
    by: String,
    /// The room a `Replace` took the place of, and the order it had without shuffle.
    before: Shared,
    sorted: Vec<u64>,
    until: u64,
}

/// The host's side of a room: it turns what members ask into changes.
#[derive(Default)]
pub(crate) struct Host {
    pub shared: Shared,
    next_id: u64,
    /// The members that have each song.
    holders: HashMap<u64, HashSet<String>>,
    /// The members that hear each song from a member who has it.
    hearing: HashMap<u64, HashSet<String>>,
    undo: Option<Undo>,
    /// The order the queue has without shuffle, kept while the room shuffles.
    sorted: Vec<u64>,
    /// What the room's shuffles are drawn from.
    seed: u64,
}

/// A song as the room holds it: its texts within what a room carries.
pub(crate) fn clipped(mut song: RoomSong) -> RoomSong {
    let clip = |text: &mut String| {
        if let Some((end, _)) = text.char_indices().nth(TEXT) {
            text.truncate(end);
        }
    };
    clip(&mut song.title);
    clip(&mut song.artist);
    clip(&mut song.album);
    song.recording = song.recording.filter(|id| id.len() <= TEXT);
    song.audio = song.audio.filter(|hash| hash.len() <= TEXT);
    song.item = song.item.filter(|item| item.len() <= TEXT);
    song.station = song.station.filter(|stream| stream.len() <= ADDRESS);
    song.duration_millis = song.duration_millis.max(0);
    song
}

impl Host {
    fn duration(&self, id: u64) -> u64 {
        self.shared.index(id).map_or(0, |index| {
            self.shared.entries[index].song.duration_millis as u64
        })
    }

    fn timeline(&mut self, timeline: Timeline) -> Change {
        self.shared.timeline = timeline;
        Change::Timeline(timeline)
    }

    fn start(&mut self, id: Option<u64>, now: u64) -> Change {
        self.timeline(Timeline {
            current: id,
            millis: 0,
            at: now,
            playing: id.is_some(),
        })
    }

    /// The songs `by` gives the room, as its entries.
    fn entries(&mut self, by: &str, songs: Vec<RoomSong>, room: usize) -> Vec<Entry> {
        songs
            .into_iter()
            .take(ADDED_AT_ONCE.min(room))
            .map(|song| {
                self.next_id += 1;
                Entry {
                    id: self.next_id,
                    song: clipped(song),
                    by: by.to_owned(),
                }
            })
            .collect()
    }

    /// The room becomes `shared`, which members hold again from nothing.
    fn becomes(&mut self, shared: Shared) -> Vec<Change> {
        self.holders.clear();
        self.hearing.clear();
        let mut changes = vec![Change::Cleared];
        if !shared.entries.is_empty() {
            changes.push(Change::Added {
                entries: shared.entries.clone(),
                after: None,
            });
        }
        changes.push(Change::Timeline(shared.timeline));
        self.shared = Shared {
            modes: self.shared.modes,
            ..shared
        };
        changes
    }

    /// The queue's ids in a shuffled order: what stands up to `stays` keeps its place.
    fn shuffled(&mut self, mut ids: Vec<u64>, stays: usize) -> Vec<u64> {
        self.seed = self.seed.wrapping_add(self.next_id).wrapping_add(1);
        let stays = stays.min(ids.len());
        library::shuffle_order(&mut ids[stays..], self.seed);
        ids
    }

    /// What `by` asked changes; the changes are already applied here. `host` says the host
    /// itself asks.
    pub(crate) fn decide(&mut self, by: &str, host: bool, ask: Ask, now: u64) -> Vec<Change> {
        if matches!(ask, Ask::Undo) {
            let undo = (self.undo).take_if(|undo| undo.by == by && now <= undo.until);
            return undo.map_or_else(Vec::new, |undo| {
                let timeline = Timeline {
                    at: now,
                    ..undo.before.timeline
                };
                self.sorted = undo.sorted;
                self.becomes(Shared {
                    timeline,
                    ..undo.before
                })
            });
        }
        let undo = self.undo.take();
        let changes = self.decided(by, host, ask, now);
        // What changed nothing leaves the last play to be taken back.
        if changes.is_empty() {
            self.undo = undo;
        }
        changes
    }

    fn decided(&mut self, by: &str, host: bool, ask: Ask, now: u64) -> Vec<Change> {
        let timeline = self.shared.timeline;
        match ask {
            Ask::Add { songs, after } => {
                let room = ENTRIES.saturating_sub(self.shared.entries.len());
                let entries = self.entries(by, songs, room);
                if entries.is_empty() {
                    return Vec::new();
                }
                // Without shuffle they stand after the same entry.
                if self.shared.modes.shuffle {
                    let place = match after {
                        None => 0,
                        Some(id) => (self.sorted.iter().position(|sorted| *sorted == id))
                            .map_or(self.sorted.len(), |index| index + 1),
                    };
                    (self.sorted).splice(place..place, entries.iter().map(|entry| entry.id));
                }
                let added = Change::Added { entries, after };
                self.shared.apply(&added);
                vec![added]
            }
            Ask::Replace { songs, start } => {
                let mut entries = self.entries(by, songs, ENTRIES);
                let Some(first) = entries.get(start).or(entries.first()) else {
                    return Vec::new();
                };
                let current = Some(first.id);
                let before = self.shared.clone();
                let sorted = entries.iter().map(|entry| entry.id).collect::<Vec<_>>();
                // A room that shuffles plays the song the play starts at, then the rest in
                // a shuffled order.
                if self.shared.modes.shuffle {
                    let start = start.min(entries.len() - 1);
                    entries.swap(0, start);
                    let order = entries.iter().map(|entry| entry.id).collect();
                    let order = Change::Ordered {
                        ids: self.shuffled(order, 1),
                    };
                    let mut shuffled = Shared {
                        entries,
                        ..Default::default()
                    };
                    shuffled.apply(&order);
                    entries = shuffled.entries;
                }
                let changes = self.becomes(Shared {
                    entries,
                    timeline: Timeline {
                        current,
                        millis: 0,
                        at: now,
                        playing: true,
                    },
                    ..Default::default()
                });
                self.undo = Some(Undo {
                    by: by.to_owned(),
                    before: Shared {
                        timeline: Timeline {
                            millis: timeline.position(now),
                            ..timeline
                        },
                        ..before
                    },
                    sorted: std::mem::replace(&mut self.sorted, sorted),
                    until: now + UNDO_MILLIS,
                });
                changes
            }
            Ask::Remove { ids } => {
                let ids = ids
                    .into_iter()
                    .filter(|id| self.shared.index(*id).is_some())
                    .collect::<Vec<_>>();
                if ids.is_empty() {
                    return Vec::new();
                }
                // A removed current song gives way to the next one that stays.
                let next = timeline.current.filter(|id| ids.contains(id)).map(|id| {
                    let from = self.shared.index(id).unwrap_or_default();
                    self.shared.entries[from..]
                        .iter()
                        .map(|entry| entry.id)
                        .find(|id| !ids.contains(id))
                });
                let removed = Change::Removed { ids };
                self.shared.apply(&removed);
                if let Change::Removed { ids } = &removed {
                    for id in ids {
                        self.holders.remove(id);
                        self.hearing.remove(id);
                    }
                    self.sorted.retain(|id| !ids.contains(id));
                }
                match next {
                    None => vec![removed],
                    Some(next) => {
                        let moved_on = self.timeline(Timeline {
                            current: next,
                            millis: 0,
                            at: now,
                            playing: timeline.playing && next.is_some(),
                        });
                        vec![removed, moved_on]
                    }
                }
            }
            Ask::Move { ids, after } => {
                if after.is_some_and(|id| ids.contains(&id)) {
                    return Vec::new();
                }
                let moved = Change::Moved { ids, after };
                self.shared.apply(&moved);
                vec![moved]
            }
            Ask::Jump { id } => match self.shared.index(id) {
                Some(_) => vec![self.start(Some(id), now)],
                None => Vec::new(),
            },
            Ask::Play if timeline.playing => Vec::new(),
            Ask::Play => {
                let current = timeline
                    .current
                    .or_else(|| self.shared.entries.first().map(|entry| entry.id));
                match current {
                    None => Vec::new(),
                    Some(_) => vec![self.timeline(Timeline {
                        current,
                        at: now,
                        playing: true,
                        ..timeline
                    })],
                }
            }
            Ask::Pause if !timeline.playing => Vec::new(),
            Ask::Pause => vec![self.timeline(Timeline {
                millis: timeline.position(now),
                at: now,
                playing: false,
                ..timeline
            })],
            Ask::Seek { id, millis } if timeline.current == Some(id) => {
                let length = self.duration(id);
                vec![self.timeline(Timeline {
                    millis: if length > 0 {
                        millis.min(length)
                    } else {
                        millis
                    },
                    at: now,
                    ..timeline
                })]
            }
            Ask::Skip { from } if timeline.current == Some(from) => self.moves_on(now, false),
            Ask::Back { from } if timeline.current == Some(from) => {
                let before = self
                    .shared
                    .index(from)
                    .and_then(|index| index.checked_sub(1))
                    .map(|index| self.shared.entries[index].id)
                    .filter(|_| timeline.position(now) <= RESTART_MILLIS);
                vec![self.start(Some(before.unwrap_or(from)), now)]
            }
            Ask::Clear if host => {
                self.holders.clear();
                self.hearing.clear();
                self.sorted.clear();
                self.shared = Shared {
                    modes: self.shared.modes,
                    ..Default::default()
                };
                vec![Change::Cleared]
            }
            Ask::Repeat { mode } if mode != self.shared.modes.repeat => {
                self.shared.modes.repeat = mode;
                vec![Change::Modes(self.shared.modes)]
            }
            Ask::Shuffle { on, seed } if on != self.shared.modes.shuffle => {
                let ids = (self.shared.entries.iter())
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>();
                let ids = if on {
                    self.seed = seed;
                    self.sorted = ids.clone();
                    let stays = (timeline.current)
                        .and_then(|id| self.shared.index(id))
                        .map_or(0, |index| index + 1);
                    self.shuffled(ids, stays)
                } else {
                    std::mem::take(&mut self.sorted)
                };
                let ordered = Change::Ordered { ids };
                self.shared.apply(&ordered);
                self.shared.modes.shuffle = on;
                vec![ordered, Change::Modes(self.shared.modes)]
            }
            Ask::AutoDj { on } if host && on != self.shared.modes.auto_dj => {
                self.shared.modes.auto_dj = on;
                vec![Change::Modes(self.shared.modes)]
            }
            Ask::Seek { .. }
            | Ask::Skip { .. }
            | Ask::Back { .. }
            | Ask::Clear
            | Ask::Undo
            | Ask::Repeat { .. }
            | Ask::Shuffle { .. }
            | Ask::AutoDj { .. } => Vec::new(),
        }
    }

    /// The room moves on by itself: its song ended, or nobody has it.
    pub(crate) fn advance(&mut self, now: u64) -> Vec<Change> {
        let heard = (self.shared.timeline.current)
            .is_some_and(|id| self.holders.get(&id).is_some_and(|held| !held.is_empty()));
        self.moves_on(now, heard)
    }

    /// The room moves to its next song, or stops at the end of its last. A song that
    /// `ended` plays again where the room repeats it; a room that repeats its queue goes
    /// back to its first song, unless nobody has any of them.
    fn moves_on(&mut self, now: u64, ended: bool) -> Vec<Change> {
        let timeline = self.shared.timeline;
        let Some(current) = timeline.current else {
            return Vec::new();
        };
        let repeat = self.shared.modes.repeat;
        let following = || {
            let after = self.shared.index(current)? + 1;
            let entries = &self.shared.entries;
            let around = repeat == QueueRepeatMode::All
                && self.holders.values().any(|held| !held.is_empty());
            (entries.get(after))
                .or_else(|| entries.first().filter(|_| around))
                .map(|entry| entry.id)
        };
        let next = match repeat {
            QueueRepeatMode::One if ended => Some(current),
            _ => following(),
        };
        vec![match next {
            Some(next) => self.start(Some(next), now),
            None => self.timeline(Timeline {
                millis: timeline.position(now),
                at: now,
                playing: false,
                ..timeline
            }),
        }]
    }

    /// When the room moves on by itself: at the end of its song, or shortly after the turn of
    /// a song nobody has.
    pub(crate) fn due(&self) -> Option<u64> {
        let timeline = self.shared.timeline;
        let current = timeline.current.filter(|_| timeline.playing)?;
        let length = self.duration(current);
        let end = (length > 0).then(|| timeline.at + length.saturating_sub(timeline.millis));
        let unheard = (self.holders.get(&current))
            .is_none_or(HashSet::is_empty)
            .then_some(timeline.at + UNHEARD_MILLIS);
        end.into_iter().chain(unheard).min()
    }

    /// `member` says which songs it has and which it lacks.
    pub(crate) fn has(&mut self, member: &str, has: &[u64], lacks: &[u64]) {
        for id in has.iter().filter(|id| self.shared.index(**id).is_some()) {
            (self.holders.entry(*id).or_default()).insert(member.to_owned());
        }
        for id in lacks {
            if let Some(holders) = self.holders.get_mut(id) {
                holders.remove(member);
            }
        }
    }

    /// `member` hears a song it lacks from a member who has it.
    pub(crate) fn hears(&mut self, member: &str, id: u64) {
        if self.shared.index(id).is_some() {
            (self.hearing.entry(id).or_default()).insert(member.to_owned());
        }
    }

    /// The members other than `member` that have a song.
    pub(crate) fn holders(&self, id: u64, member: &str) -> Vec<String> {
        let mut holders = (self.holders.get(&id).into_iter().flatten())
            .filter(|holder| *holder != member)
            .cloned()
            .collect::<Vec<_>>();
        holders.sort();
        holders
    }

    /// Whether `member` neither has the room's current song nor hears it from another.
    pub(crate) fn sits_out(&self, member: &str) -> bool {
        self.shared.timeline.current.is_some_and(|id| {
            let among = |members: &HashMap<u64, HashSet<String>>| {
                members
                    .get(&id)
                    .is_some_and(|members| members.contains(member))
            };
            !(among(&self.holders) || among(&self.hearing))
        })
    }

    pub(crate) fn left(&mut self, member: &str) {
        for members in self.holders.values_mut().chain(self.hearing.values_mut()) {
            members.remove(member);
        }
    }

    /// How many members have each song, in the queue's order.
    pub(crate) fn counts(&self) -> Vec<u32> {
        (self.shared.entries.iter())
            .map(|entry| self.holders.get(&entry.id).map_or(0, HashSet::len) as u32)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(title: &str, seconds: i64) -> RoomSong {
        RoomSong {
            title: title.into(),
            duration_millis: seconds * 1000,
            ..Default::default()
        }
    }

    fn titles(shared: &Shared) -> Vec<&str> {
        (shared.entries.iter())
            .map(|entry| entry.song.title.as_str())
            .collect()
    }

    /// A guest that applies the host's changes holds the same room.
    fn guest_follows(guest: &mut Shared, changes: &[Change], host: &Host) {
        for change in changes {
            guest.apply(change);
        }
        assert_eq!(titles(guest), titles(&host.shared));
        assert_eq!(guest.timeline, host.shared.timeline);
    }

    fn play(titles: &[&str]) -> Ask {
        Ask::Replace {
            songs: titles.iter().map(|title| song(title, 200)).collect(),
            start: 0,
        }
    }

    #[test]
    fn playing_in_a_room_replaces_its_queue_and_can_be_undone() {
        let mut host = Host::default();
        let mut guest = Shared::default();
        let changes = host.decide("ada", true, play(&["a", "b", "c"]), 0);
        guest_follows(&mut guest, &changes, &host);
        let before = host.shared.entries.clone();
        host.has("ada", &[before[0].id], &[]);

        // Mia plays two songs from the second, 30 s into the room's first.
        let played = Ask::Replace {
            songs: vec![song("x", 100), song("y", 100)],
            start: 1,
        };
        let changes = host.decide("mia", false, played, 30_000);
        guest_follows(&mut guest, &changes, &host);
        assert_eq!(titles(&host.shared), ["x", "y"]);
        assert_eq!(
            host.shared.timeline.current,
            Some(host.shared.entries[1].id)
        );
        assert_eq!(host.counts(), [0, 0]);
        // Only the member that played takes it back, and what changes nothing keeps that.
        assert!(host.decide("ada", true, Ask::Undo, 31_000).is_empty());
        assert!(host.decide("ada", true, Ask::Play, 31_000).is_empty());
        let changes = host.decide("mia", false, Ask::Undo, 35_000);
        guest_follows(&mut guest, &changes, &host);
        assert_eq!(host.shared.entries, before);
        assert_eq!(
            host.shared.timeline,
            Timeline {
                current: Some(before[0].id),
                millis: 30_000,
                at: 35_000,
                playing: true
            }
        );
        assert!(host.decide("mia", false, Ask::Undo, 36_000).is_empty());
    }

    #[test]
    fn a_play_is_not_taken_back_late_or_after_the_room_changed() {
        let mut host = Host::default();
        host.decide("ada", true, play(&["a"]), 0);
        host.decide("mia", false, play(&["x"]), 1000);
        assert!(
            host.decide("mia", false, Ask::Undo, 1001 + UNDO_MILLIS)
                .is_empty()
        );
        host.decide("mia", false, play(&["y"]), 20_000);
        host.decide("ada", true, Ask::Pause, 21_000);
        assert!(host.decide("mia", false, Ask::Undo, 22_000).is_empty());
        assert_eq!(titles(&host.shared), ["y"]);
    }

    #[test]
    fn the_room_repeats_and_shuffles_for_everyone() {
        let mut host = Host::default();
        let mut guest = Shared::default();
        let names = ["a", "b", "c", "d", "e", "f", "g", "h"];
        let changes = host.decide("ada", true, play(&names), 0);
        guest_follows(&mut guest, &changes, &host);
        let ids = (host.shared.entries.iter())
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        host.has("ada", &ids, &[]);
        let changes = host.decide("ada", true, Ask::Jump { id: ids[1] }, 10);
        guest_follows(&mut guest, &changes, &host);

        // Shuffling leaves what played and the current song where they are.
        let shuffle = |on| Ask::Shuffle { on, seed: 7 };
        let changes = host.decide("mia", false, shuffle(true), 20);
        guest_follows(&mut guest, &changes, &host);
        assert_eq!(titles(&host.shared)[..2], ["a", "b"]);
        assert_ne!(titles(&host.shared), names);
        assert!(guest.modes.shuffle);
        // A song added meanwhile stands after the same song once the shuffle ends.
        let added = Ask::Add {
            songs: vec![song("x", 200)],
            after: Some(ids[1]),
        };
        let changes = host.decide("mia", false, added, 30);
        guest_follows(&mut guest, &changes, &host);
        let changes = host.decide("ada", true, shuffle(false), 40);
        guest_follows(&mut guest, &changes, &host);
        assert_eq!(
            titles(&host.shared),
            ["a", "b", "x", "c", "d", "e", "f", "g", "h"]
        );

        // Repeating one song starts it again at its end; a skip still moves on.
        let repeat = |mode| Ask::Repeat { mode };
        host.decide("mia", false, repeat(QueueRepeatMode::One), 50);
        host.advance(200_000);
        assert_eq!(host.shared.timeline.current, Some(ids[1]));
        assert_eq!(host.shared.timeline.at, 200_000);
        host.decide("mia", false, Ask::Skip { from: ids[1] }, 200_100);
        assert_ne!(host.shared.timeline.current, Some(ids[1]));
        // Repeating the queue goes from its last song to its first.
        host.decide("mia", false, repeat(QueueRepeatMode::All), 200_200);
        host.decide("mia", false, Ask::Jump { id: ids[7] }, 200_300);
        host.advance(400_300);
        assert_eq!(host.shared.timeline.current, Some(ids[0]));
        assert!(host.shared.timeline.playing);
        // Only the host turns Auto DJ on.
        assert!(
            host.decide("mia", false, Ask::AutoDj { on: true }, 400_400)
                .is_empty()
        );
        let changes = host.decide("ada", true, Ask::AutoDj { on: true }, 400_400);
        assert_eq!(changes, [Change::Modes(host.shared.modes)]);
        assert!(host.shared.modes.auto_dj);
    }

    #[test]
    fn two_members_skipping_the_same_song_skip_it_once() {
        let mut host = Host::default();
        host.decide("ada", true, play(&["a", "b", "c"]), 0);
        let [a, b, _] = [0, 1, 2].map(|index| host.shared.entries[index].id);
        assert_eq!(
            host.decide("mia", false, Ask::Skip { from: a }, 1000).len(),
            1
        );
        assert!(
            host.decide("sam", false, Ask::Skip { from: a }, 1001)
                .is_empty()
        );
        assert_eq!(host.shared.timeline.current, Some(b));
        // Only the host clears the queue.
        assert!(host.decide("mia", false, Ask::Clear, 2000).is_empty());
        assert_eq!(
            host.decide("ada", true, Ask::Clear, 2000),
            [Change::Cleared]
        );
    }

    #[test]
    fn the_room_moves_on_at_the_end_of_a_song_and_past_one_nobody_has() {
        let mut host = Host::default();
        host.decide("ada", true, play(&["a", "b"]), 0);
        let [a, b] = [0, 1].map(|index| host.shared.entries[index].id);
        // Nobody said they have it yet.
        assert_eq!(host.due(), Some(UNHEARD_MILLIS));
        host.has("ada", &[a], &[]);
        assert_eq!(host.due(), Some(200_000));
        host.decide("ada", true, Ask::Pause, 50_000);
        assert_eq!(host.due(), None);
        host.decide("ada", true, Ask::Play, 60_000);
        assert_eq!(host.due(), Some(210_000));
        host.advance(210_000);
        assert_eq!(host.shared.timeline.current, Some(b));
        assert_eq!(host.due(), Some(210_000 + UNHEARD_MILLIS));
        // The last song ends the room's playing, and stays its current one.
        host.advance(213_000);
        assert_eq!(host.shared.timeline.current, Some(b));
        assert!(!host.shared.timeline.playing);
    }

    #[test]
    fn moved_songs_keep_their_order_and_a_removed_current_gives_way() {
        let mut host = Host::default();
        let mut guest = Shared::default();
        let changes = host.decide("ada", true, play(&["a", "b", "c", "d"]), 0);
        guest_follows(&mut guest, &changes, &host);
        let [a, b, c, d] = [0, 1, 2, 3].map(|index| host.shared.entries[index].id);
        let moved = Ask::Move {
            ids: vec![d, b],
            after: None,
        };
        let changes = host.decide("mia", false, moved, 10);
        guest_follows(&mut guest, &changes, &host);
        assert_eq!(titles(&host.shared), ["b", "d", "a", "c"]);
        let changes = host.decide("mia", false, Ask::Remove { ids: vec![a] }, 20);
        guest_follows(&mut guest, &changes, &host);
        assert_eq!(host.shared.timeline.current, Some(c));
    }
}
