//! What a room says about a song, and this library's own copy of it. A room carries text
//! only, so each listener plays what their own library holds.

use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::{Catalog, LibraryResult, TrackKey};

/// A song as a room names it: enough for another library to find its own copy.
#[derive(Clone, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct RoomSong {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_millis: i64,
    /// Its MusicBrainz recording.
    #[serde(default)]
    pub recording: Option<String>,
    /// The hash of its audio, in hex: the same file in another library.
    #[serde(default)]
    pub audio: Option<String>,
    /// What names it on the server it comes from: the same in every library that lists that
    /// server, and nothing to one that does not.
    #[serde(default)]
    pub item: Option<String>,
    /// The stream of a radio station, which every listener opens itself.
    #[serde(default)]
    pub station: Option<String>,
}

/// What stands for each source that is a server, by the source's id: the same text on every
/// device that lists that server.
pub type RoomServers = HashMap<String, String>;

fn item(servers: &RoomServers, source: Option<String>, object: Option<String>) -> Option<String> {
    let mut named = blake3::Hasher::new();
    named.update(servers.get(&source?)?.as_bytes());
    named.update(&[0]);
    named.update(object?.as_bytes());
    Some(named.finalize().to_hex().chars().take(32).collect())
}

/// How far two copies of one song may differ in length.
const SAME_LENGTH_MILLIS: i64 = 2_000;

fn spoken(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

impl Catalog {
    /// These tracks as a room names them, each with its address here; a track the Store no
    /// longer holds has no song.
    pub async fn room_songs(
        &self,
        keys: &[TrackKey],
        servers: &RoomServers,
    ) -> LibraryResult<Vec<Option<(RoomSong, String)>>> {
        let mut connection = self.acquire_reader().await?;
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            TrackKey,
            String,
            String,
            String,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        )> = sqlx::query_as(
            "SELECT track.track_key,track.title,track.display_artist,track.display_album,track.duration_millis,
                 jsonb_extract(details.metadata,'$.musicbrainz_recording_id'),lower(hex(details.audio_hash)),
                 source.object_id,track.object_id,track.media_uri
             FROM tracks track LEFT JOIN track_details details USING(track_key)
                 LEFT JOIN sources source USING(source_key)
             WHERE track.track_key IN (SELECT value FROM json_each(?1))",
        )
        .bind(serde_json::to_string(keys)?)
        .fetch_all(&mut *connection)
        .await?;
        let songs = rows
            .into_iter()
            .map(
                |(
                    key,
                    title,
                    artist,
                    album,
                    duration_millis,
                    recording,
                    audio,
                    source,
                    object,
                    uri,
                )| {
                    let song = RoomSong {
                        title,
                        artist,
                        album,
                        duration_millis,
                        recording: recording.filter(|id| !id.is_empty()),
                        audio: audio.filter(|hash| !hash.is_empty()),
                        item: item(servers, source, object),
                        station: crate::station_stream(&uri),
                    };
                    (key, (song, uri))
                },
            )
            .collect::<HashMap<_, _>>();
        Ok(keys.iter().map(|key| songs.get(key).cloned()).collect())
    }

    /// The songs this library has, to find its own copy of what a room names. Only tracks a
    /// source lists count, so a song another room named is never taken for a copy. One read
    /// of the library serves a whole room.
    pub async fn own_songs(&self, servers: &RoomServers) -> LibraryResult<OwnSongs> {
        let mut connection = self.acquire_reader().await?;
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            String,
            String,
            String,
            i64,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT track.media_uri,track.title,track.display_artist,track.duration_millis,
                 jsonb_extract(details.metadata,'$.musicbrainz_recording_id'),lower(hex(details.audio_hash)),
                 source.object_id,track.object_id
             FROM tracks track LEFT JOIN track_details details USING(track_key)
                 LEFT JOIN sources source USING(source_key)
             WHERE track.indexed=1 AND track.hidden_since IS NULL
             ORDER BY track.track_key",
        )
        .fetch_all(&mut *connection)
        .await?;
        let mut own = OwnSongs::default();
        for (uri, title, artist, duration, recording, hash, source, object) in rows {
            let uri: Arc<str> = uri.into();
            if let Some(item) = item(servers, source, object) {
                own.by_item.insert(item, uri.clone());
            }
            if let Some(recording) = recording.filter(|id| !id.is_empty()) {
                own.by_recording.entry(recording).or_insert(uri.clone());
            }
            if let Some(hash) = hash.filter(|hash| !hash.is_empty()) {
                own.by_audio.entry(hash).or_insert(uri.clone());
            }
            (own.by_name
                .entry((spoken(&title), spoken(&artist)))
                .or_default())
            .push((duration, uri));
        }
        Ok(own)
    }
}

/// The songs a library has, by what a room can name them by.
#[derive(Default)]
pub struct OwnSongs {
    by_item: HashMap<String, Arc<str>>,
    by_recording: HashMap<String, Arc<str>>,
    by_audio: HashMap<String, Arc<str>>,
    by_name: HashMap<(String, String), Vec<(i64, Arc<str>)>>,
}

impl OwnSongs {
    /// This library's own copy of a song, as its address: the same item of a server both
    /// list, else the same recording, else the same audio, else the same title and artist at
    /// the same length.
    pub fn copy(&self, song: &RoomSong) -> Option<&str> {
        let named = || {
            (self
                .by_name
                .get(&(spoken(&song.title), spoken(&song.artist)))?
                .iter())
            .find(|(duration, _)| (duration - song.duration_millis).abs() <= SAME_LENGTH_MILLIS)
            .map(|(_, uri)| uri)
        };
        (song.item.as_ref())
            .and_then(|item| self.by_item.get(item))
            .or_else(|| (song.recording.as_ref()).and_then(|id| self.by_recording.get(id)))
            .or_else(|| (song.audio.as_ref()).and_then(|hash| self.by_audio.get(hash)))
            .or_else(named)
            .map(|uri| &**uri)
    }
}
