//! Quick mix: the tracks each entry's next press plays, which its page lists under a mosaic of
//! the albums they start with, and the entries Home shows, those that match something.

use std::sync::Arc;

use sqlx::SqliteConnection;

use crate::catalog::views::{Context, Inputs, shuffled, smart_period};
use crate::{
    Catalog, FolderKey, LibraryResult, ReadCancellation, SmartPlaylistDefinition,
    SmartPlaylistSort, TrackKey,
};

/// The most entries Home shows.
pub const HOME_QUICK_MIX: usize = 6;

/// The most albums an entry's cover shows.
const COVER_ALBUMS: usize = 4;

impl Context<'_> {
    /// An entry's tracks in music folder `folder`, each with its place in seed `seed`'s
    /// shuffle, and how many of the shuffle a press takes. A limited entry in an order of its
    /// own is cut to its limit in that order first; a random one takes the first of the shuffle.
    pub(crate) fn quick_mix_tracks(
        &self,
        definition: &SmartPlaylistDefinition,
        inputs: &Inputs,
        folder: Option<u32>,
        seed: i64,
    ) -> (Vec<(i64, u32)>, Option<usize>) {
        if definition.limit.is_some() && definition.sort_field != SmartPlaylistSort::Random {
            let tracks = self
                .evaluate(definition, inputs, folder)
                .into_iter()
                .filter_map(|slot| {
                    Some((shuffled(self.snapshot.tracks.live(slot)?.key, seed), slot))
                })
                .collect();
            return (tracks, None);
        }
        let tracks = self
            .matching(definition, inputs, folder)
            .map(|(slot, record)| (shuffled(record.key, seed), slot))
            .collect();
        (tracks, definition.limit)
    }
}

/// The media URIs of `covers`, track keys in their order: a mosaic names its picks by them, and
/// only the Store holds them.
async fn cover_uris(
    connection: &mut SqliteConnection,
    covers: &[impl serde::Serialize],
) -> LibraryResult<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT track.media_uri FROM json_each(?1) requested JOIN tracks track ON track.track_key=requested.value ORDER BY CAST(requested.key AS INTEGER)",
    )
    .bind(serde_json::to_string(covers)?)
    .fetch_all(&mut *connection)
    .await?)
}

/// An entry's mosaic on its page: `picks`, media URIs, composed as a smart playlist's.
fn quick_mix_binding(
    definition: &SmartPlaylistDefinition,
    entry: u32,
    picks: Vec<String>,
) -> Option<Vec<u8>> {
    crate::artwork::mosaic_binding(
        definition
            .source_id
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        format!("quick-mix:{entry}"),
        picks,
    )
}

/// The listen windows `entries` read, read into inputs at `now`.
async fn inputs_of(
    catalog: &Catalog,
    connection: &mut SqliteConnection,
    entries: &[SmartPlaylistDefinition],
    now: i64,
) -> LibraryResult<Inputs> {
    let mut inputs = Inputs::default();
    let periods = entries.iter().filter_map(smart_period).collect();
    catalog
        .read_periods(connection, &mut inputs, periods, now)
        .await?;
    Ok(inputs)
}

impl Catalog {
    /// The first [`HOME_QUICK_MIX`] of `entries`, by place, that match a track in music folder
    /// `folder` at `now`: the ones Home shows. An entry is read only up to its first match.
    pub(crate) async fn quick_mix_filled(
        &self,
        connection: &mut SqliteConnection,
        folder: Option<FolderKey>,
        entries: &[SmartPlaylistDefinition],
        now: i64,
    ) -> LibraryResult<Vec<u32>> {
        let inputs = inputs_of(self, connection, entries, now).await?;
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot);
        let Some(folder) = context.folder_slot(folder, None) else {
            return Ok(Vec::new());
        };
        Ok(entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| context.matching(entry, &inputs, folder).next().is_some())
            .take(HOME_QUICK_MIX)
            .map(|(entry, _)| entry as u32)
            .collect())
    }

    /// The tracks a press of `entry` plays in music folder `folder`, in seed `seed`'s shuffle
    /// at `now`, and the albums they start with.
    pub async fn quick_mix_press(
        &self,
        folder: Option<FolderKey>,
        entry: &SmartPlaylistDefinition,
        seed: i64,
        now: i64,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<QuickMixPress> {
        let inputs = {
            let mut connection = self.acquire_loaded(cancellation).await?;
            inputs_of(self, &mut connection, std::slice::from_ref(entry), now).await?
        };
        // The Catalog is read before the Store; its guard holds no await.
        let (tracks, covers) = {
            let snapshot = self.snapshot();
            let context = Context::new(&snapshot);
            let Some(folder) = context.folder_slot(folder, None) else {
                return Ok(QuickMixPress::default());
            };
            let (mut tracks, take) = context.quick_mix_tracks(entry, &inputs, folder, seed);
            tracks.sort_unstable();
            if let Some(take) = take {
                tracks.truncate(take);
            }
            // The first track of each album the press starts with: the picks of its mosaic.
            let mut albums = Vec::with_capacity(COVER_ALBUMS);
            let mut covers = Vec::with_capacity(COVER_ALBUMS);
            for (_, slot) in &tracks {
                if albums.len() == COVER_ALBUMS {
                    break;
                }
                if let Some(album) = snapshot
                    .tracks
                    .live(*slot)
                    .and_then(|record| context.album_of(record))
                    && !albums.contains(&album)
                {
                    albums.push(album);
                    covers.extend(snapshot.tracks.key_of(*slot));
                }
            }
            let tracks = tracks
                .into_iter()
                .filter_map(|(_, slot)| snapshot.tracks.key_of(slot))
                .map(TrackKey::from_raw)
                .collect::<Arc<[_]>>();
            (tracks, covers)
        };
        let mut connection = self.acquire_loaded(cancellation).await?;
        let covers = cover_uris(&mut connection, &covers).await?;
        Ok(QuickMixPress { tracks, covers })
    }

    /// A Quick mix page's header for entry `entry` of `source_id`, named `name`: what `press`
    /// lists, counted in music folder `folder`, under a mosaic of the tracks it starts with, as
    /// a smart playlist's header shows its members.
    pub fn quick_mix_header(
        &self,
        entry: u32,
        name: String,
        definition: SmartPlaylistDefinition,
        press: &QuickMixPress,
        folder: Option<FolderKey>,
    ) -> crate::Header {
        let snapshot = self.snapshot();
        let context = Context::new(&snapshot);
        // While the page holds its tracks, a download change counts them again.
        self.hold_quick_mix_page(entry, &press.tracks);
        let mut totals = crate::catalog::views::Totals::default();
        if let Some(folder) = context.folder_slot(folder, None) {
            for key in press.tracks.iter() {
                if let Some(record) = snapshot
                    .tracks
                    .get(key.raw())
                    .filter(|record| context.in_source_folder(record, folder))
                {
                    totals.add_member(record);
                }
            }
        }
        let summary = totals.summary();
        let artwork_binding = quick_mix_binding(&definition, entry, press.covers.clone());
        crate::Header::QuickMix(Box::new(QuickMixRow {
            entry,
            name,
            definition,
            track_count: summary.track_count,
            duration_millis: summary.duration_millis,
            downloaded_count: summary.downloaded_count,
            artwork_binding,
        }))
    }
}

/// A Quick mix page's header: the smart playlist header's facts for an entry that is no
/// smart playlist of the Store.
#[derive(Clone, Debug, PartialEq)]
pub struct QuickMixRow {
    /// The entry's place among the Quick mix entries.
    pub entry: u32,
    pub name: String,
    pub definition: SmartPlaylistDefinition,
    pub track_count: i64,
    pub duration_millis: i64,
    pub downloaded_count: i64,
    pub artwork_binding: Option<Vec<u8>>,
}

/// What a Quick mix press plays, and the media URI of the first track of each album it starts
/// with.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuickMixPress {
    pub tracks: Arc<[TrackKey]>,
    pub covers: Vec<String>,
}
