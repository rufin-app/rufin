//! Search over the resident Catalog, and the rows of server search results by object id.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::catalog::views::{Context, holds};
use crate::read::Members;
use crate::{
    AlbumKey, AlbumRow, ArtistKey, ArtistRow, Catalog, Collection, FolderKey, Item, LibraryField,
    LibraryResult, ListKind, ListRead, MemberRead, Read, ReadCancellation, Row, SourceKey,
    TrackKey, TrackRow,
};

const SEARCH_LIMIT: usize = 100;
const SEARCH_TEXT_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SearchMode {
    #[default]
    Title,
    Lyrics,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LyricsSnippet {
    pub parts: Vec<LyricsSnippetPart>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct LyricsSnippetPart {
    pub text: String,
    pub highlighted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchRequest {
    query: String,
    limit: usize,
    on_device: bool,
}

impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self::with_limit(query, 20)
    }

    pub fn with_limit(query: impl Into<String>, limit: usize) -> Self {
        let query = names::folded(query.into().trim())
            .chars()
            .take(SEARCH_TEXT_LIMIT)
            .collect();
        Self {
            query,
            limit: limit.clamp(1, SEARCH_LIMIT),
            on_device: false,
        }
    }

    /// Finds only the tracks with a copy on this device, as a search does while their server
    /// does not answer. Albums and artists are found as before.
    pub fn on_device(mut self, on_device: bool) -> Self {
        self.on_device = on_device;
        self
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub const fn limit(&self) -> usize {
        self.limit
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchResults {
    pub tracks: Vec<TrackRow>,
    pub albums: Vec<AlbumRow>,
    pub artists: Vec<ArtistRow>,
    pub lyrics: BTreeMap<crate::TrackKey, LyricsSnippet>,
}

/// How well a row named `name` answers `query`, best first: its name is the query, starts with
/// it, has a word that starts with it, or holds it; then a name that holds more of the query's
/// `terms`, each a text and whether it must be a whole word. A row whose name holds none of
/// the query matched by another field.
fn rank(name: &str, query: &str, terms: &[(&str, bool)]) -> (u8, usize) {
    let name = names::folded(name);
    let starts_word = || {
        name.match_indices(query).any(|(at, _)| {
            name.get(..at)
                .and_then(|before| before.chars().next_back())
                .is_none_or(|before| !before.is_alphanumeric())
        })
    };
    let class = if name == query {
        0
    } else if name.starts_with(query) {
        1
    } else if starts_word() {
        2
    } else if name.contains(query) {
        3
    } else {
        4
    };
    let missing = terms
        .iter()
        .filter(|(term, word)| !holds(&[&name], None, term, *word))
        .count();
    (class, missing)
}

impl Catalog {
    /// The rows of a server's search results by object id in music folder `folder`, kept to it
    /// as the lists of that folder hold them unless the server `kept` its results to it already
    /// (its Catalog may not hold every result's folder yet).
    pub async fn search_rows_by_objects(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_artists: bool,
        track_objects: &[String],
        album_objects: &[String],
        artist_objects: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<SearchResults> {
        if [
            track_objects.len(),
            album_objects.len(),
            artist_objects.len(),
        ]
        .into_iter()
        .any(|len| len > SEARCH_LIMIT)
        {
            return Err(crate::LibraryError::InvalidRequest(
                "Search identity page exceeds 100 rows".into(),
            ));
        }
        let mut connection = self.acquire_loaded(cancellation).await?;
        let tracks = self
            .track_keys_by_objects(&mut connection, Some(source), track_objects)
            .await?;
        let keys = {
            let snapshot = self.snapshot();
            let owner = snapshot
                .source_slot(source)
                .unwrap_or(crate::catalog::snapshot::NO_SOURCE);
            let keys = |kind, objects: &[String]| {
                let objects = (objects.iter()).map(|object| (owner, object.as_str()));
                snapshot
                    .object_keys(kind, &objects.collect::<Vec<_>>())
                    .into_iter()
                    .flatten()
                    .collect()
            };
            let mut keys: [Vec<i64>; 3] = [
                tracks,
                keys(crate::CatalogKind::Album, album_objects),
                keys(crate::CatalogKind::Artist, artist_objects),
            ];
            let context = Context::new(&snapshot);
            // The server searched the folder; a folder the Catalog does not hold finds none.
            if context.folder_slot(folder, Some(owner)).is_none() {
                keys = Default::default();
            }
            keys
        };
        self.search_rows(&mut connection, &keys, album_artists, folder)
            .await
    }

    /// The rows of search results, in one statement.
    pub(crate) async fn search_rows(
        &self,
        connection: &mut sqlx::SqliteConnection,
        keys: &[Vec<i64>; 3],
        album_artists: bool,
        folder: Option<FolderKey>,
    ) -> LibraryResult<SearchResults> {
        let items = keys[0]
            .iter()
            .map(|key| Item::Track {
                key: TrackKey::from_raw(*key),
                entry: None,
            })
            .chain(
                keys[1]
                    .iter()
                    .map(|key| Item::Album(AlbumKey::from_raw(*key))),
            )
            .chain(keys[2].iter().map(|key| Item::Artist {
                key: ArtistKey::from_raw(*key),
                album_artist: album_artists,
            }))
            .collect::<Vec<_>>();
        // Each row counts in music folder `folder` as the one rule narrows its kind, with the
        // plays of every kind, as Search orders by any list's sort.
        let shape = crate::read::Shape {
            activity: true,
            folder,
            ..crate::read::Shape::of(None)
        };
        let rows = self.rows_of_on(connection, &items, &shape, true).await?;
        let mut results = SearchResults::default();
        for row in rows {
            match row {
                Row::Track(row) => results.tracks.push(row),
                Row::Album(row) => results.albums.push(row),
                Row::Artist(row) => results.artists.push(row),
                _ => {}
            }
        }
        Ok(results)
    }

    /// `results` read again from the current Catalog: the same rows with their current
    /// totals, favorites, ratings and downloads.
    pub async fn reread_search(
        &self,
        results: &SearchResults,
        folder: Option<FolderKey>,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<SearchResults> {
        let mut connection = self.acquire_loaded(cancellation).await?;
        let keys = [
            results
                .tracks
                .iter()
                .map(|row| row.track_key.raw())
                .collect(),
            results
                .albums
                .iter()
                .map(|row| row.album_key.raw())
                .collect(),
            results
                .artists
                .iter()
                .map(|row| row.artist_key.raw())
                .collect(),
        ];
        let album_artists = results.artists.iter().any(|row| row.album_artist);
        let mut reread = self
            .search_rows(&mut connection, &keys, album_artists, folder)
            .await?;
        reread.lyrics.clone_from(&results.lyrics);
        Ok(reread)
    }

    /// Tracks, albums and artists of a source whose texts hold every search term.
    pub async fn search(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        album_artists: bool,
        request: &SearchRequest,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<SearchResults> {
        if request.query.is_empty() {
            return Ok(SearchResults::default());
        }
        // A one-letter term matches a whole word; a longer one matches anywhere.
        let terms = request
            .query
            .split_whitespace()
            .map(|term| (term, term.chars().count() == 1))
            .collect::<Vec<_>>();
        let tracks = MemberRead {
            folder,
            downloaded_only: request.on_device,
            ..MemberRead::new(Collection::Source(source))
        };
        let tracks = self.order(&Read::Members(tracks), cancellation).await?;
        let albums = ListRead {
            folder,
            ..ListRead::new(ListKind::Albums, Some(source), LibraryField::Title)
        };
        let albums = self.order(&Read::List(albums), cancellation).await?;
        let artists = ListRead {
            folder,
            ..ListRead::new(
                ListKind::Artists {
                    album_artist: album_artists,
                },
                Some(source),
                LibraryField::Title,
            )
        };
        let artists = self.order(&Read::List(artists), cancellation).await?;
        let keys =
            {
                let snapshot = self.snapshot();
                let context = Context::new(&snapshot);
                let all = |holds: &dyn Fn(&str, bool) -> bool| {
                    terms.iter().all(|(term, word)| holds(term, *word))
                };
                // The best answers, each by the rank `ranked` gives a row that holds every term:
                // rows of one rank keep the order of their list by title.
                let named = |name: &str| rank(name, &request.query, &terms);
                let found = |order: &crate::Order, ranked: &dyn Fn(u32) -> Option<(u8, usize)>| {
                    match &order.0.members {
                        Members::Tracks(keys) | Members::Keys(keys) => {
                            let mut found = keys
                                .iter()
                                .filter_map(|key| Some((ranked(*key)?, *key)))
                                .collect::<Vec<_>>();
                            found.sort_by_key(|(rank, _)| *rank);
                            found
                                .into_iter()
                                .map(|(_, key)| i64::from(key))
                                .take(request.limit)
                                .collect()
                        }
                        _ => Vec::new(),
                    }
                };
                [
                    found(&tracks, &|key| {
                        let record = snapshot.tracks.get(key.into())?;
                        all(&|term, word| context.track_holds(record, term, word))
                            .then(|| named(snapshot.text(record.title)))
                    }),
                    found(&albums, &|key| {
                        let record = snapshot.albums.get(key.into())?;
                        all(&|term, word| context.album_holds(record, term, word))
                            .then(|| named(snapshot.text(record.title)))
                    }),
                    found(&artists, &|key| {
                        let record = snapshot.artists.get(key.into())?;
                        let name = snapshot.text(record.name);
                        all(&|term, word| holds(&[name], None, term, word)).then(|| named(name))
                    }),
                ]
            };
        let mut connection = self.acquire_loaded(cancellation).await?;
        self.search_rows(&mut connection, &keys, album_artists, folder)
            .await
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_name_ranks_by_how_it_holds_the_query() {
        let rank = |name, query: &str| {
            let terms = query
                .split_whitespace()
                .map(|term| (term, term.chars().count() == 1))
                .collect::<Vec<_>>();
            super::rank(name, query, &terms)
        };
        let mut names = [
            "Zombie",
            "Glove",
            "All You Need Is Love",
            "Lovely Day",
            "Love",
            "Amour",
        ];
        names.sort_by_key(|name| rank(name, "love"));
        assert_eq!(
            names,
            [
                "Love",
                "Lovely Day",
                "All You Need Is Love",
                "Glove",
                "Zombie",
                "Amour"
            ]
        );
        // A name that holds more of the query's terms comes first among the rest.
        assert!(rank("The Wall", "pink wall") < rank("Animals", "pink wall"));
        assert_eq!(rank("Beyoncé", "beyonce").0, 0);
    }
}
