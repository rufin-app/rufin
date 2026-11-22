//! Checks media locations for cleanup and playback failure recovery.
use super::SourceOwner;
use crate::runtime::source::LocalFolder;
use crate::string_error;
use std::collections::{HashMap, HashSet};

impl SourceOwner {
    /// The Local folder a track's file is in, when that folder cannot be read.
    pub async fn unavailable_local_folder(&self, media_uri: &str) -> Option<LocalFolder> {
        let cue = library::cue_media_parts(media_uri);
        let uri = cue.as_ref().map_or(media_uri, |(_, uri, _, _)| uri);
        // A Flatpak reads the file through its folder grant, at a path other than its identity.
        let access = self
            .shared
            .catalog
            .playables_by_uri(&[uri.to_owned()])
            .await
            .ok()
            .and_then(|playables| playables.into_iter().next())
            .and_then(|playable| playable.local_uri());
        let path = library::file_media_path(access.as_deref().unwrap_or(uri))?;
        self.check_local_folders().await;
        self.list_sources()
            .local_folders
            .into_iter()
            .find(|folder| folder.unavailable && path.starts_with(&folder.path))
    }

    /// Looks at each Local folder, and publishes the sources when one stopped or started
    /// being readable or its place on the host became known. A folder that cannot be read is
    /// where the tracks read under it say: a sandbox's grant names it only while it holds.
    pub(crate) async fn check_local_folders(&self) {
        let Some(local) = self
            .shared
            .settings
            .load()
            .sources
            .configured
            .iter()
            .find(|source| source.configuration.is_local())
            .map(|source| source.configuration.clone())
        else {
            return;
        };
        let mut found = HashMap::new();
        for root in super::local_roots(&local).unwrap_or_default() {
            let path = root.to_string_lossy().into_owned();
            let unavailable = tokio::fs::read_dir(&root).await.is_err();
            let location = if unavailable {
                self.shared
                    .catalog
                    .local_root_location(local.source_id.as_str(), &path)
                    .await
                    .ok()
                    .flatten()
            } else {
                tokio::task::spawn_blocking(move || library::playlist_host_path(&root))
                    .await
                    .ok()
            };
            let location = location.map_or_else(
                || path.clone(),
                |location| location.to_string_lossy().into_owned(),
            );
            found.insert(path, (location, unavailable));
        }
        {
            let mut folders = self
                .shared
                .local_folders
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if *folders == found {
                return;
            }
            *folders = found;
        }
        self.shared.configured();
    }

    pub(crate) async fn missing_media(&self, uris: &[String]) -> Result<HashSet<String>, String> {
        let catalog = &self.shared.catalog;
        let mut missing = HashSet::new();
        let source_uris: Vec<_> = uris
            .iter()
            .filter_map(|uri| {
                library::source_entity_parts(uri)
                    .filter(|(_, kind, _)| kind == "track")
                    .map(|(source, _, _)| (uri.clone(), source))
            })
            .collect();
        let catalog_missing: std::collections::HashSet<_> = catalog
            .catalog_missing_media(&source_uris)
            .await
            .map_err(string_error)?
            .into_iter()
            .collect();
        let mut playables = catalog
            .playables_by_uri(uris)
            .await
            .map_err(string_error)?
            .into_iter()
            .map(|playable| (playable.media_uri.clone(), playable))
            .collect::<std::collections::HashMap<_, _>>();
        // The copies playback looks for: the download, the track's own file and its mapped file.
        let copies = uris
            .iter()
            .map(|uri| {
                let playable = playables.get(uri);
                let own = playable.and_then(library::Playable::local_uri).or_else(|| {
                    let backing = library::cue_media_parts(uri).map(|(_, backing, _, _)| backing);
                    Some(backing.unwrap_or_else(|| uri.clone())).filter(|_| direct_local_media(uri))
                });
                let paths = playable
                    .and_then(|playable| playable.download.as_ref())
                    .map(|download| download.uri.clone())
                    .into_iter()
                    .chain(own)
                    .filter_map(|uri| location(&uri))
                    .collect::<Vec<_>>();
                (paths, playable.and_then(|playable| playable.mapped.clone()))
            })
            .collect::<Vec<_>>();
        let paths = tokio::task::spawn_blocking(move || {
            copies
                .into_iter()
                .map(|(mut paths, mapped)| {
                    paths.extend(mapped.and_then(|mapped| mapped.locate()));
                    paths
                })
                .collect()
        })
        .await
        .map_err(string_error)?;
        let available = sources::local_files_available(paths)
            .await
            .map_err(string_error)?;
        for (index, uri) in uris.iter().enumerate() {
            if available[index] {
                continue;
            }
            if direct_local_media(uri) || catalog_missing.contains(uri) {
                missing.insert(uri.clone());
                continue;
            }
            let Some((source, kind, _)) = library::source_entity_parts(uri) else {
                continue;
            };
            if kind != "track"
                || !self
                    .configuration(&source)
                    .is_some_and(|source| source.is(library::SourceKind::file_library))
            {
                continue;
            }
            let Some(file) = playables.remove(uri).and_then(|playable| playable.file) else {
                continue;
            };
            if !self
                .client(&source)?
                .media_file_exists(&file.path)
                .await
                .map_err(string_error)?
            {
                missing.insert(uri.clone());
            }
        }
        Ok(missing)
    }
}

/// Where a file or document URI is on this device.
fn location(uri: &str) -> Option<library::LocalMediaLocation> {
    if uri.starts_with("content:") {
        Some(library::LocalMediaLocation::Document(uri.to_owned()))
    } else {
        library::file_media_path(uri).map(library::LocalMediaLocation::File)
    }
}

fn direct_local_media(uri: &str) -> bool {
    let backing = library::cue_media_parts(uri).map(|(_, backing, _, _)| backing);
    let backing = backing.as_deref().unwrap_or(uri);
    library::file_media_path(backing).is_some()
        || library::document_media_id(backing).is_some()
        || backing.starts_with("content:")
}
