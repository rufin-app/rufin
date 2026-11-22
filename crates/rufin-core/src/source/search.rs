use crate::runtime::SelectedLibrary;
use library::{
    LibraryError, LibraryResult, ReadCancellation, SearchMode, SearchRequest, SearchResults,
};

/// How many results of each kind a search shows.
const SEARCH_LIMIT: usize = 60;

/// What `query` finds in the selected source and music folder: titles, from its server when it
/// searches live and `live` asks, else from the Catalog; or lyrics, in every source, the
/// selected one in its music folder. While the server does not answer, the Catalog finds only
/// the tracks with a copy on this device: a found track plays alone, with nothing to pass on to.
pub(crate) async fn search(
    owner: &super::SourceOwner,
    selected: Option<&SelectedLibrary>,
    mode: SearchMode,
    query: String,
    live: bool,
    cancel: &ReadCancellation,
) -> LibraryResult<SearchResults> {
    let catalog = &owner.shared.catalog;
    let request = SearchRequest::with_limit(query.as_str(), SEARCH_LIMIT);
    if mode == SearchMode::Lyrics {
        let folder = selected.and_then(|selected| selected.music_folder_key);
        return catalog
            .search_tracks_by_lyrics(&request, folder, cancel)
            .await;
    }
    let selected =
        selected.ok_or_else(|| LibraryError::InvalidRequest("No source selected".into()))?;
    if live
        && selected.source_id.as_str() != sources::LOCAL_LIBRARY_SOURCE_ID
        && let Some(source) = owner.cached_client(&selected.source_id)
    {
        let folder = selected
            .music_folder_key
            .zip(selected.music_folder_object_id.as_deref());
        let live = source.live_search(catalog, &query, SEARCH_LIMIT, folder);
        let result = tokio::select! {
            result = live => result,
            () = cancel.cancelled() => return Err(LibraryError::ReadCancelled),
        };
        match result {
            Ok(rows) => return Ok(rows),
            // The Catalog answers instead. A server that does not connect shows as unreachable.
            Err(sources::SourceError::Network(error)) => {
                tracing::debug!(%error, "the server did not answer a search");
            }
            Err(error) => tracing::warn!(%error, "the server could not search"),
        }
    }
    // A server with a folder mapping plays its tracks from this device.
    let on_device = owner
        .cached_client(&selected.source_id)
        .is_some_and(|source| source.is_unreachable())
        && !owner
            .shared
            .settings
            .load()
            .sources
            .configured
            .iter()
            .any(|configured| {
                configured.configuration.source_id == selected.source_id
                    && configured.local_access.is_some()
            });
    catalog
        .search(
            selected.source_key,
            selected.music_folder_key,
            false,
            &request.on_device(on_device),
            cancel,
        )
        .await
}
