use crate::runtime::SelectedLibrary;
use library::{
    LibraryError, LibraryResult, ReadCancellation, SearchFound, SearchMode, SearchRequest,
};

/// What a search found and who found it. A server's results come a page of each kind at a
/// time and stay the set and order it gave; the Catalog's are every match.
#[derive(Clone, Debug, Default)]
pub(crate) struct Searched {
    pub found: SearchFound,
    /// Where the server's next page of each kind (tracks, albums, artists) starts, for a kind
    /// it may hold more of; None when the Catalog found the results.
    pub next: Option<[Option<usize>; 3]>,
}

/// What `query` finds in the selected source and music folder: titles, from its server when it
/// searches live, else from the Catalog; or lyrics, which the Catalog matches.
pub(crate) async fn search(
    owner: &super::SourceOwner,
    selected: Option<&SelectedLibrary>,
    mode: SearchMode,
    query: &str,
    cancel: &ReadCancellation,
) -> LibraryResult<Searched> {
    if mode == SearchMode::Title
        && let Some(selected) = selected
    {
        match server_page(owner, selected, query, [Some(0); 3], cancel).await? {
            Some(Ok(page)) => {
                return Ok(Searched {
                    found: SearchFound {
                        keys: page.keys,
                        lyrics: None,
                    },
                    next: Some(page.more.map(|more| more.then_some(library::SEARCH_PAGE))),
                });
            }
            // The Catalog answers instead. A server that does not connect shows as unreachable.
            Some(Err(sources::SourceError::Network(error))) => {
                tracing::debug!(%error, "the server did not answer a search");
            }
            Some(Err(error)) => tracing::warn!(%error, "the server could not search"),
            None => {}
        }
    }
    Ok(Searched {
        found: matched(owner, selected, mode, query, cancel).await?,
        next: None,
    })
}

/// A page of what the server of the selected source finds for `query`: of each kind `offsets`
/// asks for, the results from that offset. None for a source with no server to search.
pub(crate) async fn server_page(
    owner: &super::SourceOwner,
    selected: &SelectedLibrary,
    query: &str,
    offsets: [Option<usize>; 3],
    cancel: &ReadCancellation,
) -> LibraryResult<Option<sources::SourceResult<sources::SearchPage>>> {
    if selected.source_id.as_str() == sources::LOCAL_LIBRARY_SOURCE_ID {
        return Ok(None);
    }
    let Some(source) = owner.cached_client(&selected.source_id) else {
        return Ok(None);
    };
    let folder = selected
        .music_folder_key
        .zip(selected.music_folder_object_id.as_deref());
    tokio::select! {
        result = source.live_search(&owner.shared.catalog, query, offsets, folder) => {
            Ok(Some(result))
        }
        () = cancel.cancelled() => Err(LibraryError::ReadCancelled),
    }
}

/// Everything `query` matches in the Catalog: titles in the selected source and music folder,
/// or lyrics in every source, the selected one in its music folder. While the server does not
/// answer, titles match only the tracks with a copy on this device: a found track plays alone,
/// with nothing to pass on to.
pub(crate) async fn matched(
    owner: &super::SourceOwner,
    selected: Option<&SelectedLibrary>,
    mode: SearchMode,
    query: &str,
    cancel: &ReadCancellation,
) -> LibraryResult<SearchFound> {
    let catalog = &owner.shared.catalog;
    let request = SearchRequest::new(query);
    if mode == SearchMode::Lyrics {
        let folder = selected.and_then(|selected| selected.music_folder_key);
        return catalog
            .search_tracks_by_lyrics(&request, folder, cancel)
            .await;
    }
    let selected =
        selected.ok_or_else(|| LibraryError::InvalidRequest("No source selected".into()))?;
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
            &request.on_device(on_device),
            cancel,
        )
        .await
}
