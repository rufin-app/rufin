use super::*;
use rufin_core::settings::app::{RecentSearchKind, RecentSearchResult};

/// How many results of each kind a search reads as it opens; the rest read as the page scrolls.
const FIRST_RESULTS: usize = 64;

/// Search results `range` of `category` of the Search page `view`; each row shows the row
/// fields of its category's list, as GTK's search does.
async fn search_rows(
    view: &rufin_core::view::View,
    fields: &[LibraryField],
    category: CollectionCategory,
    range: std::ops::Range<usize>,
    cancel: &library::ReadCancellation,
) -> Result<Vec<AndroidBrowseRow>, AndroidError> {
    let first = range.start;
    let rows = (view.search_rows(category, range, cancel).await).map_err(error)?;
    Ok(rows
        .into_iter()
        .enumerate()
        .map(|(index, (row, lyrics_snippet))| AndroidBrowseRow {
            lyrics_snippet,
            ..shown_row(row, fields, first + index)
        })
        .collect())
}

impl AndroidLibrary {
    /// The results the Search page `read` reads, with what commits while it reads.
    async fn found<F>(
        self: Arc<Self>,
        read: impl FnOnce(library::ReadCancellation) -> F + Send + 'static,
    ) -> Result<AndroidSearch, AndroidError>
    where
        F: std::future::Future<Output = library::LibraryResult<rufin_core::view::Opened>>
            + Send
            + 'static,
    {
        let reading = AndroidReading::new(self.source.reading());
        let fields = {
            let settings = self.settings.load();
            CollectionCategory::ALL.map(|category| settings.library_list(category.key()).row_fields)
        };
        let library = Arc::clone(&self);
        run_read(&self.runtime, move |cancel| async move {
            let view = read(cancel.clone()).await.map_err(error)?.view;
            let mut kinds = Vec::new();
            for category in CollectionCategory::ALL {
                let fields = &fields[category as usize];
                let rows = search_rows(&view, fields, category, 0..FIRST_RESULTS, &cancel).await?;
                let (count, more) = view.search_len(category);
                kinds.push(AndroidSearchKind {
                    category,
                    rows,
                    count: count as u64,
                    more,
                });
            }
            Ok(AndroidSearch {
                kinds,
                view: library.view_of(view),
                reading,
            })
        })
        .await
    }
}

/// What a search found of one category: its first rows, how many it found so far, and whether
/// its server may hold more, which rows past them ask for.
#[derive(uniffi::Record)]
pub struct AndroidSearchKind {
    pub category: CollectionCategory,
    pub rows: Vec<AndroidBrowseRow>,
    pub count: u64,
    pub more: bool,
}

/// A search's results by category, the Search page that found them and what committed while
/// it searched.
#[derive(uniffi::Record)]
pub struct AndroidSearch {
    pub kinds: Vec<AndroidSearchKind>,
    pub view: Arc<AndroidView>,
    pub reading: Arc<AndroidReading>,
}

#[uniffi::export]
impl AndroidView {
    /// A Search page's results of `category` from `offset`, at most `limit` and 256, and what
    /// committed while they read. Fewer than `limit` are the last ones.
    pub async fn search_rows(
        self: Arc<Self>,
        category: CollectionCategory,
        offset: u64,
        limit: u32,
    ) -> Result<AndroidRows, AndroidError> {
        let runtime = self.runtime.clone();
        let reading = AndroidReading::new(self.source.reading());
        let fields = (self.settings.load())
            .library_list(category.key())
            .row_fields;
        run_read(&runtime, move |cancel| async move {
            let offset = usize::try_from(offset).map_err(error)?;
            let range = offset..offset + limit.min(256) as usize;
            Ok(AndroidRows {
                rows: search_rows(&self.view, &fields, category, range, &cancel).await?,
                reading,
            })
        })
        .await
    }
}

#[uniffi::export]
impl AndroidLibrary {
    /// Searches `text` in the library selected now; the page it opens says when a committed
    /// change, also one that committed while it searched, means it reads its results again.
    pub async fn search(
        self: Arc<Self>,
        text: String,
        mode: library::SearchMode,
    ) -> Result<AndroidSearch, AndroidError> {
        let options = OpenOptions {
            filter: text,
            search_mode: mode,
            layout: Some(LibraryLayout::Row),
            ..Default::default()
        };
        let selected = self.source.selected_library();
        let source = self.source.clone();
        self.found(move |cancel| async move {
            source
                .open(
                    Route::Search,
                    selected,
                    options,
                    library::RouteSeedWindow::top(),
                    true,
                    &cancel,
                )
                .await
        })
        .await
    }

    /// Reads the Search page `view` again, as its refresh asked.
    pub async fn search_again(
        self: Arc<Self>,
        view: Arc<AndroidView>,
    ) -> Result<AndroidSearch, AndroidError> {
        let view = Arc::clone(&view.view);
        self.found(move |cancel| async move {
            view.reread(
                view.options().clone(),
                library::RouteSeedWindow::top(),
                &cancel,
            )
            .await
        })
        .await
    }

    /// The remembered search results as saved, each with the library row it names while the
    /// library holds it.
    pub async fn recent_search_results(&self) -> Result<Vec<AndroidRecentResult>, AndroidError> {
        let recent = self.settings.load().recent_search_results.clone();
        let catalog = self.catalog.clone();
        run_read(&self.runtime, move |cancel| async move {
            catalog.loaded().await;
            let uris = recent
                .iter()
                .map(|row| row.media_uri.clone())
                .collect::<Vec<_>>();
            let tracks = catalog
                .tracks_by_uris(&uris, &cancel)
                .await
                .map_err(error)?;
            let collections = catalog.items(&uris.iter().map(String::as_str).collect::<Vec<_>>());
            let items = uris
                .iter()
                .zip(collections)
                .filter_map(|(uri, item)| {
                    item.or_else(|| {
                        let track = tracks.iter().find(|track| track.media_uri == *uri)?;
                        Some(library::Item::Track {
                            key: track.track_key,
                            entry: None,
                        })
                    })
                })
                .collect::<Vec<_>>();
            let mut rows = catalog
                .rows_of(&items, true, &cancel)
                .await
                .map_err(error)?;
            Ok(recent
                .into_iter()
                .map(|result| {
                    let row = rows.iter().position(|row| match row {
                        Row::Track(track) => track.media_uri == result.media_uri,
                        Row::Album(album) => album.media_uri == result.media_uri,
                        Row::Artist(artist) => artist.media_uri == result.media_uri,
                        _ => false,
                    });
                    AndroidRecentResult {
                        route: Route::of_uri(&result.media_uri),
                        row: row.map(|at| rows.swap_remove(at).into()),
                        result,
                    }
                })
                .collect())
        })
        .await
    }

    pub fn remember_search_result(&self, row: RecentSearchResult) -> Result<bool, AndroidError> {
        self.settings.remember_search_result(row).map_err(error)
    }

    pub fn remove_search_result(
        &self,
        kind: RecentSearchKind,
        media_uri: String,
    ) -> Result<bool, AndroidError> {
        self.settings
            .remove_search_result(kind, &media_uri)
            .map_err(error)
    }

    pub fn clear_search_results(&self) -> Result<bool, AndroidError> {
        self.settings.clear_search_results().map_err(error)
    }
}
