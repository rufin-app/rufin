//! Listening together. One member hosts a room: it owns the room's queue and timeline and
//! decides what the others ask of them. Every member, the host included, mirrors both into
//! its own player and plays its own copies; a room carries text, never audio.
mod state;

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use base64::Engine as _;
use rufin_connect::{
    EndpointAddr, IrohRelay,
    media::PeerByteResponse,
    room::{Link, MediaAsk, Room},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tracing::warn;

pub(crate) use state::{
    ADDED_AT_ONCE, Ask, Change, ENTRIES, Entry, Modes, Shared, Timeline, clipped,
};
use state::{Host, MEMBERS, UNDO_MILLIS, VERSION};

use crate::{connect::ActionError, playback::PlaybackOwner, settings::SettingsOwner, string_error};

const NAME: usize = 40;
const SNAPSHOT: usize = 200;
const ANSWER: Duration = Duration::from_secs(10);
const TICK: Duration = Duration::from_secs(5);
/// How long a member whose connection to the room ended looks for the way back.
const BACK: Duration = Duration::from_secs(60);
/// How many members a listener is told can send it a song.
const SOURCES: usize = 3;

/// What the player needs of the room it follows: the room as this device holds it, and a
/// way to ask the room for something.
pub(crate) struct Session {
    shared: Mutex<Shared>,
    local: mpsc::UnboundedSender<Local>,
    /// The room's endpoint: members ask each other for audio over it.
    room: Arc<Room>,
    /// What lets a member into the room, which a member asking for audio shows.
    key: String,
    /// The room shares songs with listeners who don't have them.
    sharing: AtomicBool,
    /// The songs this device hears from another member, as the room was told.
    heard: Mutex<HashSet<u64>>,
    /// This device hosts the room.
    pub(crate) hosting: bool,
}

enum Local {
    Ask(Ask),
    Remove(String),
    /// What the room shows of this device's settings changed.
    Shown,
    /// The host turns sharing songs on or off.
    Share(bool),
    /// Who can send this song's audio: answered with where they are.
    Want(u64, oneshot::Sender<Vec<EndpointAddr>>),
    /// This device hears this song from another member.
    Hears(u64),
    /// The time to take back this listener's play, by its count, is over.
    Lapsed(u64),
}

/// What a member asks another for: a song of the room as a stream, encoded as the asker
/// plays it, from a time on.
#[derive(Serialize, Deserialize)]
struct MediaWant {
    key: String,
    entry: u64,
    encoding: library::AudioEncoding,
    #[serde(default)]
    window: Option<(u64, Option<u64>)>,
}

/// What stands in a player's queue for a room's song its library has no copy of.
pub(crate) fn lacking_uri(song: &library::RoomSong) -> String {
    let named = format!(
        "{}\n{}\n{}\n{}",
        song.title, song.artist, song.album, song.duration_millis
    );
    format!("rufin-room:{}", blake3::hash(named.as_bytes()).to_hex())
}

impl Session {
    pub(crate) fn read<T>(&self, read: impl FnOnce(&Shared) -> T) -> T {
        read(&self.shared.lock().unwrap_or_else(|p| p.into_inner()))
    }

    pub(crate) fn ask(&self, ask: Ask) {
        let _ = self.local.send(Local::Ask(ask));
    }

    pub(crate) fn sharing(&self) -> bool {
        self.sharing.load(Ordering::Relaxed)
    }

    /// The room's song that stands in this player's queue as `uri`, where another member
    /// can send it: the room shares songs and someone has it.
    pub(crate) fn lacking(&self, uri: &str) -> Option<u64> {
        if !self.sharing() {
            return None;
        }
        self.read(|shared| {
            (shared.entries.iter())
                .find(|entry| lacking_uri(&entry.song) == uri)
                .map(|entry| entry.id)
        })
    }

    /// A song this library lacks, as a member who has it streams it from `window` on.
    pub(crate) async fn audio(
        &self,
        entry: u64,
        encoding: library::AudioEncoding,
        window: Option<(u64, Option<u64>)>,
    ) -> Result<PeerByteResponse, String> {
        let nobody = || localization::tr("Nobody in the room can send this song");
        if !self.sharing() {
            return Err(nobody());
        }
        let (answer, holders) = oneshot::channel();
        let _ = self.local.send(Local::Want(entry, answer));
        let holders = tokio::time::timeout(ANSWER, holders)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        let ask = serde_json::to_value(MediaWant {
            key: self.key.clone(),
            entry,
            encoding,
            window,
        })
        .map_err(string_error)?;
        let mut failed = nobody();
        for holder in holders {
            match self.room.fetch(holder, &ask).await {
                Ok(audio) => {
                    let first =
                        (self.heard.lock().unwrap_or_else(|p| p.into_inner())).insert(entry);
                    if first {
                        let _ = self.local.send(Local::Hears(entry));
                    }
                    return Ok(audio);
                }
                Err(error) => failed = error.to_string(),
            }
        }
        Err(failed)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RoomMember {
    pub id: String,
    pub name: String,
    pub host: bool,
    #[serde(default)]
    pub you: bool,
    /// The member has no copy of the room's current song.
    pub sitting_out: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RoomView {
    pub name: String,
    pub hosting: bool,
    pub invite: String,
    pub members: Vec<RoomMember>,
    /// The room's songs, and how many of them this library has.
    pub songs: u32,
    pub have: u32,
    /// How many members have each song and the name of who added it, in the queue's order.
    pub counts: Vec<u32>,
    pub added_by: Vec<String>,
    /// Where the room is reached, in words.
    pub reach: String,
    /// The room is reached through a relay this device does not use for its own rooms yet.
    pub relay_offered: bool,
    /// Members who have a song send it to listeners who don't, while it plays.
    pub sharing: bool,
    /// The listener just played something here, which Undo still takes back.
    pub undo: bool,
}

/// Something the room tells the listener once.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RoomNotice {
    pub serial: u64,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RoomStatus {
    pub room: Option<RoomView>,
    pub notice: Option<RoomNotice>,
    /// What the listener was called in the last room, to ask for it once.
    pub your_name: String,
    /// Where a room this device creates is reached, in words.
    pub reach: String,
    /// This device is joining a room, which leaving gives up.
    pub joining: bool,
}

/// Where a room on `relay` is reached, in words.
fn reach(relay: &IrohRelay) -> String {
    match relay {
        IrohRelay::Off => localization::tr("On this network only"),
        IrohRelay::Public => localization::tr("Through public relays"),
        IrohRelay::Custom { url } => localization::tr_with(
            "Through {relay}",
            &[("relay", relay.host().as_deref().unwrap_or(url))],
        ),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum RoomAction {
    Create {
        name: String,
    },
    Join {
        invite: String,
        name: String,
    },
    Leave,
    /// The host takes a member out of the room.
    Remove {
        member: String,
    },
    Undo,
    /// This device's own rooms go through the relay of the room it listens in.
    UseRelay,
    /// The host lets members who have a song send it to listeners who don't, or stops it.
    Share {
        songs: bool,
    },
}

#[derive(Clone, Serialize, Deserialize)]
struct Invite {
    v: u32,
    host: EndpointAddr,
    key: String,
    room: String,
    /// What opens the room's relay, where it is locked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    /// The room shared songs when the invite was made: a listener reads it before joining.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    sharing: bool,
}

/// The page an invite link opens. The invite stands after the `#`, which a browser keeps to
/// itself: the page hands it to the app and the site is told nothing of the room.
const INVITE_PAGE: &str = "https://rufin.app/room";

impl Invite {
    fn link(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        format!(
            "{INVITE_PAGE}#{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
        )
    }

    /// Reads an invite as it was shared: its link, or what the page hands the app,
    /// `rufin://room/` and the invite.
    fn read(text: &str) -> Result<Self, String> {
        let invalid = || localization::tr("This is not a room invite");
        let data = text.trim().rsplit(['/', '#']).next().unwrap_or_default();
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(data)
            .map_err(|_| invalid())?;
        let invite: Self = serde_json::from_slice(&json).map_err(|_| invalid())?;
        if invite.v != VERSION {
            return Err(localization::tr(
                "This room uses another version of Rufin. Please update Rufin.",
            ));
        }
        Ok(invite)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ToHost {
    Hello {
        version: u32,
        key: String,
        name: String,
        /// Where other members reach this one for audio.
        #[serde(default)]
        address: Option<EndpointAddr>,
    },
    Ping {
        sent: u64,
    },
    Has {
        has: Vec<u64>,
        lacks: Vec<u64>,
    },
    Ask {
        ask: Ask,
    },
    /// Who can send this song?
    Want {
        entry: u64,
    },
    /// This member hears this song from another.
    Hears {
        entry: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ToGuest {
    Welcome {
        you: String,
        room: String,
        now: u64,
        #[serde(default)]
        sharing: bool,
    },
    Refused {
        reason: String,
    },
    Pong {
        sent: u64,
        now: u64,
    },
    Members {
        members: Vec<RoomMember>,
    },
    /// What changed in the room and who asked for it. A change of many songs comes in
    /// several of these, all but the last saying `more`.
    Changed {
        by: Option<String>,
        changes: Vec<Change>,
        #[serde(default)]
        more: bool,
        /// The room's clock as the change left.
        now: u64,
    },
    Counts {
        counts: Vec<u32>,
    },
    /// The host turned sharing songs on or off.
    Sharing {
        on: bool,
    },
    /// Where the members are that can send a song.
    Sources {
        entry: u64,
        holders: Vec<EndpointAddr>,
    },
    Closed,
}

fn tell<T: Serialize>(send: &mpsc::Sender<Value>, message: &T) -> bool {
    serde_json::to_value(message).is_ok_and(|frame| send.try_send(frame).is_ok())
}

/// The room's changes as the frames that tell a member of them, so many songs in each.
fn changed(by: Option<String>, changes: Vec<Change>, now: u64) -> Vec<ToGuest> {
    let mut frames = vec![Vec::new()];
    let mut carried = 0;
    for change in changes {
        let Change::Added { entries, mut after } = change else {
            frames.last_mut().expect("a frame").push(change);
            continue;
        };
        for entries in entries.chunks(SNAPSHOT) {
            if carried + entries.len() > SNAPSHOT {
                frames.push(Vec::new());
                carried = 0;
            }
            carried += entries.len();
            let last = entries.last().map(|entry| entry.id);
            let entries = entries.to_vec();
            (frames.last_mut().expect("a frame")).push(Change::Added { entries, after });
            after = last;
        }
    }
    let last = frames.len() - 1;
    (frames.into_iter().enumerate())
        .map(|(index, changes)| ToGuest::Changed {
            by: by.clone().filter(|_| index == last),
            changes,
            more: index < last,
            now,
        })
        .collect()
}

fn named(name: &str) -> String {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = name.chars().take(NAME).collect::<String>();
    if name.is_empty() {
        localization::tr("Listener")
    } else {
        name
    }
}

/// What a member did, for the others to read.
fn told(name: &str, before: &Shared, shared: &Shared, changes: &[Change]) -> Option<String> {
    let say = |text: &str| Some(localization::tr_with(text, &[("name", name)]));
    let modes = shared.modes;
    if modes != before.modes {
        return say(if modes.shuffle != before.modes.shuffle {
            match modes.shuffle {
                true => "{name} turned shuffle on",
                false => "{name} turned shuffle off",
            }
        } else if modes.auto_dj != before.modes.auto_dj {
            match modes.auto_dj {
                true => "{name} turned Auto DJ on",
                false => "{name} turned Auto DJ off",
            }
        } else {
            match modes.repeat {
                library::QueueRepeatMode::Off => "{name} turned repeat off",
                library::QueueRepeatMode::All => "{name} turned repeat on",
                library::QueueRepeatMode::One => "{name} repeats this song",
            }
        });
    }
    let before = &before.timeline;
    let timeline = changes.iter().rev().find_map(|change| match change {
        Change::Timeline(timeline) => Some(*timeline),
        _ => None,
    });
    let added = changes.iter().find_map(|change| match change {
        Change::Added { entries, .. } => Some(entries),
        _ => None,
    });
    let title = |id: Option<u64>| {
        (id.and_then(|id| shared.index(id)))
            .map(|index| shared.entries[index].song.title.clone())
            .unwrap_or_default()
    };
    match (added, timeline) {
        (Some(_), Some(now)) => Some(localization::tr_with(
            "{name} played {title}",
            &[("name", name), ("title", &title(now.current))],
        )),
        (Some(entries), None) if entries.len() == 1 => Some(localization::tr_with(
            "{name} added {title}",
            &[("name", name), ("title", &entries[0].song.title)],
        )),
        (Some(entries), None) => Some(localization::tr_with(
            "{name} added {count} songs",
            &[("name", name), ("count", &entries.len().to_string())],
        )),
        (None, Some(now)) if now.current != before.current => Some(localization::tr_with(
            "{name} played {title}",
            &[("name", name), ("title", &title(now.current))],
        )),
        (None, Some(now)) if now.playing != before.playing => say(if now.playing {
            "{name} resumed"
        } else {
            "{name} paused"
        }),
        (None, None) if changes.contains(&Change::Cleared) => say("{name} cleared the queue"),
        (None, None) if changes.iter().any(|c| matches!(c, Change::Removed { .. })) => {
            say("{name} removed a song")
        }
        _ => None,
    }
}

struct Running {
    stop: CancellationToken,
    session: Arc<Session>,
    /// The relay the room is reached through.
    relay: IrohRelay,
    task: tokio::task::JoinHandle<()>,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct RoomOwner {
    settings: SettingsOwner,
    playback: Arc<PlaybackOwner>,
    runtime: tokio::runtime::Handle,
    status: watch::Sender<RoomStatus>,
    running: Mutex<Option<Running>>,
    /// What gives up the join under way.
    joining: Mutex<Option<CancellationToken>>,
    /// Creating, joining and leaving run one at a time.
    actions: tokio::sync::Mutex<()>,
}

/// One member's part of a room, as its task holds it.
struct Part {
    owner: Arc<RoomOwner>,
    session: Arc<Session>,
    /// The room as this device's player mirrors it.
    mirror: Shared,
    name: String,
    invite: String,
    /// The relay the room is reached through.
    relay: IrohRelay,
    hosting: bool,
    /// Members who have a song send it to listeners who don't.
    sharing: bool,
    you: String,
    members: Vec<RoomMember>,
    counts: Vec<u32>,
    /// The songs this library has.
    mine: HashSet<u64>,
    started: Instant,
    /// The room's clock less this task's.
    ahead: i64,
    /// This listener's plays here, and the one that can still be taken back.
    plays: u64,
    undo: Option<u64>,
}

impl Part {
    fn now(&self) -> u64 {
        (self.here() as i64 + self.ahead).max(0) as u64
    }

    /// This task's own clock. It stands still while the device sleeps.
    fn here(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn name_of(&self, id: &str) -> String {
        (self.members.iter().find(|member| member.id == id))
            .map(|member| member.name.clone())
            .unwrap_or_default()
    }

    fn show(&self) {
        let view = RoomView {
            name: self.name.clone(),
            hosting: self.hosting,
            invite: self.invite.clone(),
            members: self
                .members
                .iter()
                .map(|member| RoomMember {
                    you: member.id == self.you,
                    ..member.clone()
                })
                .collect(),
            songs: self.mirror.entries.len() as u32,
            have: (self.mirror.entries.iter())
                .filter(|entry| self.mine.contains(&entry.id))
                .count() as u32,
            counts: self.counts.clone(),
            added_by: (self.mirror.entries.iter())
                .map(|entry| self.name_of(&entry.by))
                .collect(),
            reach: reach(&self.relay),
            relay_offered: self.relay != IrohRelay::Off
                && !self.relay.same(&self.owner.settings.iroh_relay()),
            sharing: self.sharing,
            undo: self.undo.is_some(),
        };
        self.owner.status.send_if_modified(|status| {
            let changed = status.room.as_ref() != Some(&view);
            status.room = Some(view);
            changed
        });
    }

    fn notice(&self, text: String) {
        self.owner.status.send_modify(|status| {
            status.notice = Some(RoomNotice {
                serial: status.notice.as_ref().map_or(0, |notice| notice.serial) + 1,
                text,
            });
        });
    }

    /// The time to take back the play counted `play` is over.
    fn lapsed(&mut self, play: u64) {
        if self.undo == Some(play) {
            self.undo = None;
            self.show();
        }
    }

    /// Makes this device's player follow the room's changes. Answers the added songs this
    /// library has and the ones it lacks.
    async fn follow(&mut self, by: Option<&str>, changes: &[Change]) -> (Vec<u64>, Vec<u64>) {
        let was = Shared {
            entries: Vec::new(),
            ..self.mirror
        };
        let before = was.timeline;
        let (mut has, mut lacks) = (Vec::new(), Vec::new());
        let mut led = false;
        for change in changes {
            match self.owner.playback.room_changed(&self.mirror, change).await {
                Ok((found, missing)) => {
                    has.extend(found);
                    lacks.extend(missing);
                }
                Err(error) => warn!(%error, "could not follow the room's queue"),
            }
            self.mirror.apply(change);
            (self
                .session
                .shared
                .lock()
                .unwrap_or_else(|p| p.into_inner()))
            .apply(change);
            led |= matches!(change, Change::Timeline(_));
        }
        self.mine.extend(&has);
        self.mine.retain(|id| self.mirror.index(*id).is_some());
        if led {
            (self.owner.playback).follow_room(&self.mirror, self.now(), Some(before));
        }
        // This listener's own play can be taken back for a while, until the room changes
        // again.
        let played = by == Some(self.you.as_str()) && led && changes.contains(&Change::Cleared);
        self.undo = played.then(|| {
            self.plays += 1;
            let (play, local) = (self.plays, self.session.local.clone());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(UNDO_MILLIS - 1000)).await;
                let _ = local.send(Local::Lapsed(play));
            });
            play
        });
        let mut said = by
            .filter(|by| *by != self.you)
            .and_then(|by| told(&self.name_of(by), &was, &self.mirror, changes));
        // The room came to a song this library has no copy of.
        let current = self.mirror.timeline.current;
        let lacked = current.is_some_and(|id| !self.mine.contains(&id));
        // In a room that shares songs a member who has it may send it.
        if current != before.current && lacked && !self.sharing {
            let sits_out = localization::tr("Not in your library. Sitting this one out.");
            said = Some(match said {
                Some(text) => format!("{text} · {sits_out}"),
                None => sits_out,
            });
        }
        if let Some(text) = said {
            self.notice(text);
        }
        self.show();
        (has, lacks)
    }
}

impl RoomOwner {
    pub(crate) fn new(
        settings: SettingsOwner,
        playback: Arc<PlaybackOwner>,
        runtime: tokio::runtime::Handle,
    ) -> Arc<Self> {
        let status = RoomStatus {
            your_name: settings.file().load().room_name.clone(),
            reach: reach(&settings.iroh_relay()),
            ..RoomStatus::default()
        };
        let owner = Arc::new(Self {
            settings,
            playback,
            runtime,
            status: watch::channel(status).0,
            running: Mutex::new(None),
            joining: Mutex::new(None),
            actions: tokio::sync::Mutex::new(()),
        });
        crate::settings::follow(
            &owner.runtime,
            owner.settings.file(),
            Arc::downgrade(&owner),
            |owner, previous, current| {
                if previous.iroh_relay != current.iroh_relay {
                    let reach = reach(&owner.settings.iroh_relay());
                    owner.status.send_modify(|status| status.reach = reach);
                    if let Some(session) = owner.listening() {
                        let _ = session.local.send(Local::Shown);
                    }
                }
                std::future::ready(())
            },
        );
        owner
    }

    /// The room this device listens in.
    fn listening(&self) -> Option<Arc<Session>> {
        (self
            .running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref())
        .map(|running| Arc::clone(&running.session))
    }

    pub fn subscribe(&self) -> watch::Receiver<RoomStatus> {
        self.status.subscribe()
    }

    fn part(
        self: &Arc<Self>,
        session: &Arc<Session>,
        name: String,
        invite: String,
        relay: IrohRelay,
        you: String,
        hosting: bool,
    ) -> Part {
        Part {
            owner: Arc::clone(self),
            session: Arc::clone(session),
            mirror: Shared::default(),
            name,
            invite,
            relay,
            hosting,
            sharing: session.sharing(),
            you,
            members: Vec::new(),
            counts: Vec::new(),
            mine: HashSet::new(),
            started: Instant::now(),
            ahead: 0,
            plays: 0,
            undo: None,
        }
    }

    fn session(
        room: Arc<Room>,
        key: String,
        sharing: bool,
        hosting: bool,
    ) -> (Arc<Session>, mpsc::UnboundedReceiver<Local>) {
        let (local, asked) = mpsc::unbounded_channel();
        let session = Arc::new(Session {
            shared: Mutex::new(Shared::default()),
            local,
            room,
            key,
            sharing: AtomicBool::new(sharing),
            heard: Mutex::default(),
            hosting,
        });
        (session, asked)
    }

    fn run(
        self: &Arc<Self>,
        session: Arc<Session>,
        relay: IrohRelay,
        stop: CancellationToken,
        room: impl Future<Output = ()> + Send + 'static,
    ) {
        let owner = Arc::clone(self);
        let task = self.runtime.spawn(async move {
            room.await;
            owner.playback.leave_room().await;
            owner
                .running
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take();
            owner.status.send_modify(|status| status.room = None);
        });
        *self.running.lock().unwrap_or_else(|p| p.into_inner()) = Some(Running {
            stop,
            session,
            relay,
            task,
        });
    }

    async fn create(self: &Arc<Self>, name: String) -> Result<(), String> {
        let name = named(&name);
        let relay = self.settings.iroh_relay();
        let (room, joined) = Room::host(&relay).await.map_err(string_error)?;
        let host = room.address().await;
        let you = host.id.to_string();
        let key = blake3::hash(format!("{you}{}", crate::playback::random_u64()).as_bytes())
            .to_hex()
            .chars()
            .take(32)
            .collect::<String>();
        let title = localization::tr_with("{name}'s room", &[("name", &name)]);
        // A room starts sharing songs or not as the host last chose.
        let sharing = self.settings.file().load().room_shares;
        let invite = Invite {
            v: VERSION,
            host,
            key: key.clone(),
            room: title.clone(),
            token: relay.token(),
            sharing,
        };
        let room = Arc::new(room);
        let (session, asked) = Self::session(Arc::clone(&room), key.clone(), sharing, true);
        let (songs, current, millis, playing) =
            self.playback.host_room(Arc::clone(&session)).await?;
        let mut part = self.part(&session, title, invite.link(), relay, you.clone(), true);
        part.members = vec![RoomMember {
            id: you.clone(),
            name,
            host: true,
            you: true,
            sitting_out: false,
        }];
        // The host's player already holds its queue: the room starts as that queue.
        let mut state = Host::default();
        let mut after = None;
        for songs in songs.chunks(SNAPSHOT) {
            let added = Ask::Add {
                songs: songs.to_vec(),
                after,
            };
            for change in state.decide(&you, true, added, 0) {
                if let Change::Added { entries, .. } = &change {
                    after = entries.last().map(|entry| entry.id);
                    let ids = entries.iter().map(|entry| entry.id).collect::<Vec<_>>();
                    state.has(&you, &ids, &[]);
                    part.mine.extend(ids);
                }
                part.mirror.apply(&change);
            }
        }
        state.shared.timeline = Timeline {
            current: current.and_then(|index| state.shared.entries.get(index).map(|e| e.id)),
            millis,
            at: 0,
            playing,
        };
        part.mirror.timeline = state.shared.timeline;
        *session.shared.lock().unwrap_or_else(|p| p.into_inner()) = part.mirror.clone();
        part.counts = state.counts();
        part.show();
        let stop = CancellationToken::new();
        self.serve(&session, &room, &stop);
        self.run(
            session,
            part.relay.clone(),
            stop.clone(),
            host_room(part, state, room, joined, asked, invite, stop),
        );
        Ok(())
    }

    /// Answers the members that ask this device for a song's audio, while the room lasts.
    fn serve(self: &Arc<Self>, session: &Arc<Session>, room: &Room, stop: &CancellationToken) {
        let Some(mut asks) = room.media_asks() else {
            return;
        };
        let (session, stop) = (Arc::clone(session), stop.clone());
        let playback = Arc::clone(&self.playback);
        self.runtime.spawn(async move {
            loop {
                let ask = tokio::select! {
                    _ = stop.cancelled() => break,
                    ask = asks.recv() => ask,
                };
                let Some(ask) = ask else { break };
                tokio::spawn(send_audio(Arc::clone(&session), Arc::clone(&playback), ask));
            }
        });
    }

    /// Joins the room of an invite, unless leaving gives the join up first: a room that
    /// does not answer is waited for a while.
    async fn join(self: &Arc<Self>, invite: String, name: String) -> Result<(), String> {
        let given_up = CancellationToken::new();
        let waits = |token| {
            *self.joining.lock().unwrap_or_else(|p| p.into_inner()) = token;
            let joining = self
                .joining
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_some();
            self.status.send_modify(|status| status.joining = joining);
        };
        waits(Some(given_up.clone()));
        let joined = tokio::select! {
            _ = given_up.cancelled() => Ok(()),
            joined = self.joined(invite, name) => joined,
        };
        waits(None);
        joined
    }

    async fn joined(self: &Arc<Self>, invite: String, name: String) -> Result<(), String> {
        let read = Invite::read(&invite)?;
        let name = named(&name);
        let (room, mut link) =
            (Room::join(read.host.clone(), read.token.clone()).await).map_err(string_error)?;
        let room = Arc::new(room);
        let (you, now, sharing) = greet(&mut link, &read.key, &name, room.reach())
            .await
            .map_err(|refused| refused.unwrap_or_else(unreachable))?;
        let (session, asked) = Self::session(Arc::clone(&room), read.key.clone(), sharing, false);
        self.playback.join_room(Arc::clone(&session));
        let relay = room.relay().clone();
        let mut part = self.part(&session, read.room.clone(), read.link(), relay, you, false);
        part.ahead = now as i64;
        part.show();
        let stop = CancellationToken::new();
        self.serve(&session, &room, &stop);
        self.run(
            session,
            part.relay.clone(),
            stop.clone(),
            guest_room(part, room, link, asked, stop, read, name),
        );
        Ok(())
    }

    async fn leave(&self) {
        let running = self
            .running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(running) = running {
            running.stop.cancel();
            let _ = running.task.await;
        }
    }

    /// Keeps the name the listener gave, so the next room does not ask.
    fn called(&self, name: &str) -> Result<(), String> {
        let name = named(name);
        self.settings.file().update(|stored| {
            stored.room_name = name.clone();
            Ok(())
        })?;
        self.status.send_modify(|status| status.your_name = name);
        Ok(())
    }

    async fn act(self: &Arc<Self>, action: RoomAction) -> Result<(), String> {
        let session = self.listening();
        match action {
            RoomAction::Create { .. } | RoomAction::Join { .. } if session.is_some() => {
                Err(localization::tr("Leave this room first"))
            }
            RoomAction::Create { .. } | RoomAction::Join { .. } if self.playback.casting() => {
                Err(localization::tr("Stop casting to listen together"))
            }
            RoomAction::Create { name } => {
                self.called(&name)?;
                self.create(name).await
            }
            RoomAction::Join { invite, name } => {
                self.called(&name)?;
                self.join(invite, name).await
            }
            RoomAction::Leave => Ok(self.leave().await),
            RoomAction::Remove { member } => Ok({
                if let Some(session) = session {
                    let _ = session.local.send(Local::Remove(member));
                }
            }),
            RoomAction::Undo => Ok({
                if let Some(session) = session {
                    session.ask(Ask::Undo);
                }
            }),
            RoomAction::Share { songs } => Ok({
                if let Some(session) = session {
                    let _ = session.local.send(Local::Share(songs));
                }
            }),
            RoomAction::UseRelay => {
                let relay = (self
                    .running
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_ref())
                .map(|running| running.relay.clone())
                .filter(|relay| *relay != IrohRelay::Off)
                .ok_or_else(|| localization::tr("This room uses no relay"))?;
                let settings = self.settings.clone();
                tokio::task::spawn_blocking(move || settings.set_iroh_relay(relay))
                    .await
                    .map_err(string_error)?
                    .map_err(string_error)
            }
        }
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl RoomOwner {
    pub fn status(&self) -> RoomStatus {
        self.status.borrow().clone()
    }

    /// What a listener reads of an invite before joining: that its room shares songs.
    /// Nothing for a room that does not, and for text that is no invite.
    pub fn invite_note(&self, invite: String) -> String {
        match Invite::read(&invite) {
            Ok(invite) if invite.sharing => localization::tr_with(
                "{room} shares songs: members who have a song play it for listeners who don't",
                &[("room", &invite.room)],
            ),
            _ => String::new(),
        }
    }

    /// The name of the room an invite leads to, for the question an opened link asks.
    /// Nothing for text that is no invite.
    pub fn invite_room(&self, invite: String) -> String {
        Invite::read(&invite).map_or_else(|_| String::new(), |invite| invite.room)
    }

    pub async fn execute(self: &Arc<Self>, action: RoomAction) -> Result<RoomStatus, ActionError> {
        // Leaving gives up a join that is still under way.
        if matches!(action, RoomAction::Leave)
            && let Some(joining) = self
                .joining
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take()
        {
            joining.cancel();
        }
        let owner = Arc::clone(self);
        self.runtime
            .spawn(async move {
                let _action = owner.actions.lock().await;
                owner.act(action).await.map_err(ActionError::Failed)?;
                Ok(owner.status())
            })
            .await
            .map_err(|error| ActionError::Failed(error.to_string()))?
    }

    pub fn changes(&self) -> Arc<RoomSubscription> {
        let mut changes = self.subscribe();
        changes.mark_changed();
        Arc::new(RoomSubscription {
            changes: tokio::sync::Mutex::new(changes),
        })
    }
}

#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct RoomSubscription {
    changes: tokio::sync::Mutex<watch::Receiver<RoomStatus>>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl RoomSubscription {
    pub async fn next(&self) -> Result<RoomStatus, ActionError> {
        let mut changes = self.changes.lock().await;
        changes
            .changed()
            .await
            .map_err(|failure| ActionError::Failed(failure.to_string()))?;
        Ok(changes.borrow_and_update().clone())
    }
}

/// What the room says when it starts or stops sharing songs.
fn shares(on: bool) -> String {
    if on {
        localization::tr("This room shares songs with listeners who don't have them")
    } else {
        localization::tr("This room no longer shares songs")
    }
}

/// Answers a member that asks this device for a song's audio: only a member of this room,
/// only while the room shares songs, and only the song the room plays or the one after it.
async fn send_audio(session: Arc<Session>, playback: Arc<PlaybackOwner>, ask: MediaAsk) {
    let wanted = serde_json::from_value::<MediaWant>(ask.ask.clone())
        .ok()
        .filter(|want| want.key == session.key && session.sharing());
    let Some(want) = wanted else {
        return ask
            .refuse(&localization::tr("This room does not share songs"))
            .await;
    };
    let song = session.read(|shared| {
        let current = shared.timeline.current.and_then(|id| shared.index(id))?;
        let index = shared.index(want.entry)?;
        (index == current || index == current + 1).then(|| shared.entries[index].song.clone())
    });
    let Some(song) = song else {
        return ask
            .refuse(&localization::tr("The room is not playing this song"))
            .await;
    };
    match playback.room_audio(&song, want.encoding, want.window).await {
        // A member that seeks or moves on stops reading: that ends the sending, no more.
        Ok(audio) => {
            if let Err(error) = ask.answer(audio).await {
                tracing::debug!(%error, "a song stopped being sent to a member of the room");
            }
        }
        Err(error) => ask.refuse(&error).await,
    }
}

fn unreachable() -> String {
    localization::tr("The room is unreachable")
}

/// Says hello to the room's host over a new connection. Answers who this member is there,
/// the room's clock and whether it shares songs; the host's reason where it refuses, and
/// nothing where it does not answer.
async fn greet(
    link: &mut Link,
    key: &str,
    name: &str,
    address: EndpointAddr,
) -> Result<(String, u64, bool), Option<String>> {
    let hello = ToHost::Hello {
        version: VERSION,
        key: key.to_owned(),
        name: name.to_owned(),
        address: Some(address),
    };
    if !tell(&link.send, &hello) {
        return Err(None);
    }
    let answer = tokio::time::timeout(ANSWER, link.frames.recv())
        .await
        .ok()
        .flatten()
        .and_then(|frame| serde_json::from_value(frame).ok());
    match answer {
        Some(ToGuest::Welcome {
            you, now, sharing, ..
        }) => Ok((you, now, sharing)),
        Some(ToGuest::Refused { reason }) => Err(Some(reason)),
        _ => Err(None),
    }
}

/// Finds the way back into a room whose connection ended, as the member this device was
/// there, for as long as `BACK` lasts. A host that refuses ends the search.
async fn back(room: &Room, invite: &Invite, name: &str) -> Result<(Link, u64, bool), String> {
    let until = tokio::time::Instant::now() + BACK;
    loop {
        let tried = async {
            let mut link = (room.reach_host(invite.host.clone()).await).map_err(|_| None)?;
            let (_, now, sharing) = greet(&mut link, &invite.key, name, room.reach()).await?;
            Ok((link, now, sharing))
        };
        match tokio::time::timeout_at(until, tried).await {
            Ok(Ok(back)) => return Ok(back),
            Ok(Err(Some(refused))) => return Err(refused),
            Ok(Err(None)) => {}
            Err(_) => return Err(unreachable()),
        }
        // A network that is away fails at once.
        if tokio::time::timeout_at(until, tokio::time::sleep(Duration::from_secs(2)))
            .await
            .is_err()
        {
            return Err(unreachable());
        }
    }
}

enum Heard {
    Joined {
        id: String,
        /// Which of the member's connections this is: a member that comes back has a newer.
        link: u64,
        name: String,
        address: Option<EndpointAddr>,
        send: mpsc::Sender<Value>,
    },
    Said {
        id: String,
        said: ToHost,
    },
    Left {
        id: String,
        link: u64,
    },
}

/// Hears one connection: a member says hello with the room's key first, then whatever it
/// asks.
async fn hear(mut link: Link, key: String, serial: u64, heard: mpsc::Sender<Heard>) {
    let hello = tokio::time::timeout(ANSWER, link.frames.recv())
        .await
        .ok()
        .flatten()
        .and_then(|frame| serde_json::from_value(frame).ok());
    let (name, address) = match hello {
        Some(ToHost::Hello {
            version,
            key: given,
            name,
            address,
        }) if version == VERSION && given == key => (named(&name), address),
        Some(ToHost::Hello { version, .. }) => {
            let reason = if version == VERSION {
                localization::tr("This invite is no longer valid")
            } else {
                localization::tr("This room uses another version of Rufin. Please update Rufin.")
            };
            tell(&link.send, &ToGuest::Refused { reason });
            // The refusal leaves before the connection closes.
            tokio::time::sleep(Duration::from_secs(1)).await;
            return;
        }
        _ => return,
    };
    let id = link.peer.clone();
    // A member is reached for audio at the address it gives for itself, never another's.
    let address = address.filter(|address| address.id.to_string() == id);
    let joined = Heard::Joined {
        id: id.clone(),
        link: serial,
        name,
        address,
        send: link.send.clone(),
    };
    if heard.send(joined).await.is_err() {
        return;
    }
    while let Some(frame) = link.frames.recv().await {
        if let Ok(said) = serde_json::from_value(frame) {
            let said = Heard::Said {
                id: id.clone(),
                said,
            };
            if heard.send(said).await.is_err() {
                return;
            }
        }
    }
    let _ = heard.send(Heard::Left { id, link: serial }).await;
}

/// The host's room: it decides what members ask, tells every member what changed and moves
/// the room on when a song ends.
async fn host_room(
    mut part: Part,
    mut state: Host,
    room: Arc<Room>,
    mut joined: mpsc::Receiver<Link>,
    mut asked: mpsc::UnboundedReceiver<Local>,
    mut invite: Invite,
    stop: CancellationToken,
) {
    let (hearing, mut heard) = mpsc::channel(64);
    let mut guests = Vec::<(String, mpsc::Sender<Value>)>::new();
    // The connections heard so far, and the one each member speaks over.
    let mut heard_links = 0;
    let mut links = HashMap::<String, u64>::new();
    let mut tick = tokio::time::interval(TICK);
    // Where each member is reached for audio.
    let mut addresses = HashMap::from([(part.you.clone(), room.reach())]);
    // Where the members are that have a song, for `member` to ask: none unless the room
    // shares songs.
    let sources = |state: &Host, addresses: &HashMap<String, EndpointAddr>, entry, member: &str| {
        (state.holders(entry, member).iter())
            .filter_map(|holder| addresses.get(holder).cloned())
            .take(SOURCES)
            .collect::<Vec<_>>()
    };
    loop {
        let due = state
            .due()
            .map(|due| tokio::time::sleep(Duration::from_millis(due.saturating_sub(part.now()))));
        let (by, changes) = tokio::select! {
            _ = stop.cancelled() => break,
            Some(link) = joined.recv() => {
                heard_links += 1;
                tokio::spawn(hear(link, invite.key.clone(), heard_links, hearing.clone()));
                continue;
            }
            Some(heard) = heard.recv() => match heard {
                Heard::Joined { id, link, name, address, send } => {
                    // A member whose connection ended comes back as who it was.
                    let known = guests.iter().position(|(guest, _)| *guest == id);
                    if known.is_none() && guests.len() + 1 >= MEMBERS {
                        let reason = localization::tr("The room is full");
                        tell(&send, &ToGuest::Refused { reason });
                        continue;
                    }
                    let welcome = ToGuest::Welcome {
                        you: id.clone(),
                        room: part.name.clone(),
                        now: part.now(),
                        sharing: part.sharing,
                    };
                    let mut sent = tell(&send, &welcome);
                    let room = vec![
                        Change::Added { entries: state.shared.entries.clone(), after: None },
                        Change::Modes(state.shared.modes),
                        Change::Timeline(state.shared.timeline),
                    ];
                    for frame in changed(None, room, part.now()) {
                        sent &= tell(&send, &frame);
                    }
                    // Who can play each song, as the room stands: later only changes are told.
                    sent &= tell(&send, &ToGuest::Counts { counts: part.counts.clone() });
                    if sent {
                        links.insert(id.clone(), link);
                        addresses.extend(address.map(|address| (id.clone(), address)));
                        match known {
                            Some(known) => guests[known].1 = send,
                            None => {
                                let member = RoomMember { id: id.clone(), name, ..Default::default() };
                                part.members.push(member);
                                guests.push((id, send));
                            }
                        }
                    }
                    (None, Vec::new())
                }
                Heard::Said { id, said } => match said {
                    ToHost::Ping { sent } => {
                        if let Some((_, send)) = guests.iter().find(|(guest, _)| *guest == id) {
                            tell(send, &ToGuest::Pong { sent, now: part.now() });
                        }
                        continue;
                    }
                    ToHost::Has { has, lacks } => {
                        state.has(&id, &has, &lacks);
                        (None, Vec::new())
                    }
                    ToHost::Ask { ask } => {
                        // A play taken back says nothing more.
                        let by = (!matches!(ask, Ask::Undo)).then(|| id.clone());
                        (by, state.decide(&id, false, ask, part.now()))
                    }
                    ToHost::Want { entry } => {
                        let holders = if part.sharing {
                            sources(&state, &addresses, entry, &id)
                        } else {
                            Vec::new()
                        };
                        if let Some((_, send)) = guests.iter().find(|(guest, _)| *guest == id) {
                            tell(send, &ToGuest::Sources { entry, holders });
                        }
                        continue;
                    }
                    ToHost::Hears { entry } => {
                        state.hears(&id, entry);
                        (None, Vec::new())
                    }
                    ToHost::Hello { .. } => continue,
                },
                Heard::Left { id, link } => {
                    // The member came back over a newer connection.
                    if links.get(&id) != Some(&link) {
                        continue;
                    }
                    links.remove(&id);
                    guests.retain(|(guest, _)| *guest != id);
                    part.members.retain(|member| member.id != id);
                    addresses.remove(&id);
                    state.left(&id);
                    (None, Vec::new())
                }
            },
            Some(local) = asked.recv() => match local {
                Local::Ask(ask) => {
                    let by = (!matches!(ask, Ask::Undo)).then(|| part.you.clone());
                    (by, state.decide(&part.you, true, ask, part.now()))
                }
                Local::Remove(member) => {
                    guests.retain(|(guest, send)| {
                        let stays = *guest != member;
                        if !stays {
                            tell(send, &ToGuest::Closed);
                        }
                        stays
                    });
                    part.members.retain(|member_| member_.id != member || member_.host);
                    links.remove(&member);
                    addresses.remove(&member);
                    state.left(&member);
                    (None, Vec::new())
                }
                Local::Shown => (None, Vec::new()),
                Local::Share(on) => {
                    if on != part.sharing {
                        part.sharing = on;
                        part.session.sharing.store(on, Ordering::Relaxed);
                        let kept = part.owner.settings.file().update(|stored| {
                            stored.room_shares = on;
                            Ok(())
                        });
                        if let Err(error) = kept {
                            warn!(%error, "could not keep whether rooms share songs");
                        }
                        // The invite says what a listener joins.
                        invite.sharing = on;
                        part.invite = invite.link();
                        guests.retain(|(_, send)| tell(send, &ToGuest::Sharing { on }));
                    }
                    (None, Vec::new())
                }
                Local::Want(entry, answer) => {
                    let _ = answer.send(sources(&state, &addresses, entry, &part.you));
                    continue;
                }
                Local::Hears(entry) => {
                    state.hears(&part.you, entry);
                    (None, Vec::new())
                }
                Local::Lapsed(play) => {
                    part.lapsed(play);
                    continue;
                }
            },
            _ = async { due.expect("a due time").await }, if due.is_some() => {
                (None, state.advance(part.now()))
            }
            _ = tick.tick() => {
                (part.owner.playback).follow_room(&part.mirror, part.now(), None);
                continue;
            }
        };
        if !changes.is_empty() {
            let (has, lacks) = part.follow(by.as_deref(), &changes).await;
            state.has(&part.you, &has, &lacks);
            for frame in changed(by, changes, part.now()) {
                guests.retain(|(_, send)| tell(send, &frame));
            }
        }
        // A member that could not be told has left.
        part.members
            .retain(|member| member.host || guests.iter().any(|(guest, _)| *guest == member.id));
        for member in &mut part.members {
            member.sitting_out = state.sits_out(&member.id);
        }
        let counts = state.counts();
        if counts != part.counts {
            part.counts = counts;
            let counts = ToGuest::Counts {
                counts: part.counts.clone(),
            };
            guests.retain(|(_, send)| tell(send, &counts));
        }
        let members = ToGuest::Members {
            members: part.members.clone(),
        };
        guests.retain(|(_, send)| tell(send, &members));
        part.show();
    }
    for (_, send) in &guests {
        tell(send, &ToGuest::Closed);
    }
    // The goodbye leaves before the room's endpoint closes.
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(room);
}

/// A guest's room: it follows what the host tells and passes on what the listener asks.
async fn guest_room(
    mut part: Part,
    room: Arc<Room>,
    mut link: Link,
    mut asked: mpsc::UnboundedReceiver<Local>,
    stop: CancellationToken,
    invite: Invite,
    name: String,
) {
    let mut tick = tokio::time::interval(TICK);
    let mut nearest = u64::MAX;
    let mut greeted = false;
    // The host tells the whole room next, to a member that came back.
    let mut returned = false;
    // The changes of one change of the room that came so far.
    let mut batch = Vec::new();
    // Who waits to hear from the host where a song's holders are.
    let mut wanted = HashMap::<u64, Vec<oneshot::Sender<Vec<EndpointAddr>>>>::new();
    let ended = loop {
        tokio::select! {
            _ = stop.cancelled() => break None,
            frame = link.frames.recv() => {
                let Some(frame) = frame else {
                    // The connection ended and the host did not say the room did.
                    part.notice(localization::tr("Lost the room. Reconnecting..."));
                    let found = tokio::select! {
                        _ = stop.cancelled() => break None,
                        found = back(&room, &invite, &name) => found,
                    };
                    match found {
                        Ok((again, now, sharing)) => {
                            link = again;
                            part.ahead = now as i64 - part.here() as i64;
                            nearest = u64::MAX;
                            part.sharing = sharing;
                            part.session.sharing.store(sharing, Ordering::Relaxed);
                            batch.clear();
                            returned = true;
                            part.notice(localization::tr("Back in the room"));
                            continue;
                        }
                        Err(reason) => break Some(reason),
                    }
                };
                match serde_json::from_value(frame) {
                    Ok(ToGuest::Changed { by, changes, more, now }) => {
                        // The room's clock is no earlier than the host said: a device that
                        // slept catches up before it follows.
                        part.ahead = part.ahead.max(now as i64 - part.here() as i64);
                        batch.extend(changes);
                        if more {
                            continue;
                        }
                        let mut changes = std::mem::take(&mut batch);
                        if std::mem::take(&mut returned) {
                            let told = changes.iter().flat_map(|change| match change {
                                Change::Added { entries, .. } => entries.as_slice(),
                                _ => &[],
                            });
                            if told.eq(&part.mirror.entries) {
                                // The queue is the one this player holds: the host hears
                                // again which of it this library has.
                                changes.retain(|change| !matches!(change, Change::Added { .. }));
                                let (has, lacks) = (part.mirror.entries.iter())
                                    .map(|entry| entry.id)
                                    .partition(|id| part.mine.contains(id));
                                tell(&link.send, &ToHost::Has { has, lacks });
                            } else {
                                changes.insert(0, Change::Cleared);
                            }
                        }
                        let (has, lacks) = part.follow(by.as_deref(), &changes).await;
                        if !(has.is_empty() && lacks.is_empty()) {
                            tell(&link.send, &ToHost::Has { has, lacks });
                        }
                        let placed = changes.iter().any(|c| matches!(c, Change::Timeline(_)));
                        if placed && !std::mem::replace(&mut greeted, true) {
                            let have = part.mine.len().to_string();
                            let songs = part.mirror.entries.len().to_string();
                            let mut joined = localization::tr_with(
                                "You have {have} of {songs} songs",
                                &[("have", &have), ("songs", &songs)],
                            );
                            if part.sharing {
                                joined = format!("{joined} · {}", shares(true));
                            }
                            part.notice(joined);
                        }
                    }
                    Ok(ToGuest::Members { members }) => {
                        part.members = members;
                        part.show();
                    }
                    Ok(ToGuest::Counts { counts }) => {
                        part.counts = counts;
                        part.show();
                    }
                    Ok(ToGuest::Pong { sent, now }) => {
                        let here = part.here();
                        let trip = here.saturating_sub(sent);
                        // The host read its clock while this ping was away. The quickest
                        // answer places it best; one that no longer fits what is kept (the
                        // device slept, a clock drifted) places it anew.
                        let least = now as i64 - here as i64;
                        if trip <= nearest || !(least..=least + trip as i64).contains(&part.ahead) {
                            nearest = trip;
                            part.ahead = least + (trip / 2) as i64;
                        }
                    }
                    Ok(ToGuest::Sharing { on }) => {
                        part.sharing = on;
                        part.session.sharing.store(on, Ordering::Relaxed);
                        part.notice(shares(on));
                        part.show();
                    }
                    Ok(ToGuest::Sources { entry, holders }) => {
                        for waiting in wanted.remove(&entry).unwrap_or_default() {
                            let _ = waiting.send(holders.clone());
                        }
                    }
                    Ok(ToGuest::Closed) => break Some(localization::tr("The room ended")),
                    _ => {}
                }
            }
            Some(local) = asked.recv() => match local {
                Local::Ask(ask) => {
                    tell(&link.send, &ToHost::Ask { ask });
                }
                Local::Shown => part.show(),
                Local::Want(entry, answer) => {
                    wanted.entry(entry).or_default().push(answer);
                    tell(&link.send, &ToHost::Want { entry });
                }
                Local::Hears(entry) => {
                    tell(&link.send, &ToHost::Hears { entry });
                }
                Local::Lapsed(play) => part.lapsed(play),
                // Only the host removes a member or turns sharing on.
                Local::Remove(_) | Local::Share(_) => {}
            },
            _ = tick.tick() => {
                tell(&link.send, &ToHost::Ping { sent: part.here() });
                (part.owner.playback).follow_room(&part.mirror, part.now(), None);
            }
        }
    };
    if let Some(ended) = ended {
        part.notice(ended);
    }
    drop(room);
}
