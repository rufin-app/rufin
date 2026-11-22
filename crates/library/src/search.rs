//! Search over the resident Catalog, and the rows of server search results by object id.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::catalog::views::{Context, holds};
use crate::read::Members;
use crate::{
    AlbumKey, ArtistKey, Catalog, Collection, FolderKey, Item, LibraryField, LibraryResult,
    ListKind, ListRead, MemberRead, Read, ReadCancellation, Row, SourceKey, TrackKey,
};

/// How many results of a kind a server is asked for in one page of its search.
pub const SEARCH_PAGE: usize = 100;
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
    on_device: bool,
}

impl SearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        let query = names::folded(query.into().trim())
            .chars()
            .take(SEARCH_TEXT_LIMIT)
            .collect();
        Self {
            query,
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
}

/// What a search found: the keys of its tracks, albums and artists in that order, each kind
/// best first, and where the lyrics of each track matched when it searched lyrics.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchFound {
    pub keys: [Vec<i64>; 3],
    pub lyrics: Option<Arc<crate::lyrics_search::LyricsFound>>,
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
    /// The keys of a page of a server's search results by object id, each kind in the server's
    /// order: the results the Catalog holds. A music folder `folder` the Catalog does not hold
    /// finds none; the server kept its results to the folder.
    pub async fn search_keys_by_objects(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        track_objects: &[String],
        album_objects: &[String],
        artist_objects: &[String],
        cancellation: &ReadCancellation,
    ) -> LibraryResult<[Vec<i64>; 3]> {
        let mut connection = self.acquire_loaded(cancellation).await?;
        let tracks = self
            .track_keys_by_objects(&mut connection, Some(source), track_objects)
            .await?;
        let snapshot = self.snapshot();
        let owner = snapshot
            .source_slot(source)
            .unwrap_or(crate::catalog::snapshot::NO_SOURCE);
        if Context::new(&snapshot)
            .folder_slot(folder, Some(owner))
            .is_none()
        {
            return Ok(Default::default());
        }
        let keys = |kind, objects: &[String]| {
            let objects = (objects.iter()).map(|object| (owner, object.as_str()));
            snapshot
                .object_keys(kind, &objects.collect::<Vec<_>>())
                .into_iter()
                .flatten()
                .collect()
        };
        Ok([
            tracks,
            keys(crate::CatalogKind::Album, album_objects),
            keys(crate::CatalogKind::Artist, artist_objects),
        ])
    }

    /// The rows of search results `keys` of one `kind` (tracks, albums, artists), at most 256,
    /// in their order; a result the Catalog no longer holds has none. Each row counts in music
    /// folder `folder` as the one rule narrows its kind, with the plays of every kind, as
    /// Search shows the fields of any list.
    pub async fn search_rows(
        &self,
        kind: usize,
        keys: &[i64],
        folder: Option<FolderKey>,
        art: bool,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<Vec<Row>> {
        let items = keys
            .iter()
            .take(crate::read::WINDOW)
            .map(|key| match kind {
                0 => Item::Track {
                    key: TrackKey::from_raw(*key),
                    entry: None,
                },
                1 => Item::Album(AlbumKey::from_raw(*key)),
                _ => Item::Artist {
                    key: ArtistKey::from_raw(*key),
                    album_artist: false,
                },
            })
            .collect::<Vec<_>>();
        let shape = crate::read::Shape {
            activity: true,
            folder,
            ..crate::read::Shape::of(None)
        };
        let mut connection = self.acquire_loaded(cancellation).await?;
        self.rows_of_on(&mut connection, &items, &shape, art).await
    }

    /// The results of `found` the Catalog still holds, in their order.
    pub fn search_held(&self, found: &SearchFound) -> SearchFound {
        use crate::catalog::snapshot::Packed;
        let snapshot = self.snapshot();
        let held = |kind: usize, key: &i64| match kind {
            0 => (snapshot.tracks.get(*key)).is_some_and(|record| !record.deleted()),
            1 => (snapshot.albums.get(*key)).is_some_and(|record| !record.deleted()),
            _ => (snapshot.artists.get(*key)).is_some_and(|record| !record.deleted()),
        };
        SearchFound {
            keys: std::array::from_fn(|kind| {
                (found.keys[kind].iter().copied())
                    .filter(|key| held(kind, key))
                    .collect()
            }),
            lyrics: found.lyrics.clone(),
        }
    }

    /// Every track, album and artist of a source whose texts hold every search term.
    pub async fn search(
        &self,
        source: SourceKey,
        folder: Option<FolderKey>,
        request: &SearchRequest,
        cancellation: &ReadCancellation,
    ) -> LibraryResult<SearchFound> {
        if request.query.is_empty() {
            return Ok(SearchFound::default());
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
                    album_artist: false,
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
                // Every answer, best first by the rank `ranked` gives a row that holds every term:
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
                            found.into_iter().map(|(_, key)| i64::from(key)).collect()
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
        Ok(SearchFound { keys, lyrics: None })
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
