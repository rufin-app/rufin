//! The stream cache: the tracks after the playing one, fetched before playback reaches them,
//! and the played ones, kept until the cache is full. Its copies are download records only
//! the cache owns, so playback finds one as it finds a download, and a download or a rule of
//! the same track takes the copy over.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use async_channel::Sender;
use library::TrackKey;
use sources::{AudioQuality, SourceId};
use tracing::{debug, warn};

use super::{
    ActiveDownload, Actor, DownloadAudio, DownloadFailure, DownloadSubject, media_source_id,
    playable, track_download::*,
};

#[derive(Default)]
pub(super) struct Ahead {
    /// The tracks to fetch, nearest first. One stays here while it is fetched.
    pub(super) wanted: VecDeque<(SourceId, String)>,
    /// The playing track and the ones after it, which the cache does not give up.
    near: HashSet<String>,
    /// A fetch found no server; the retry tick lets the next one try.
    pub(super) waiting: bool,
    /// Hears of each track the cache has a copy of now.
    kept: Option<Sender<String>>,
}

impl Ahead {
    pub(super) fn reporting(kept: Sender<String>) -> Self {
        Self {
            kept: Some(kept),
            ..Self::default()
        }
    }

    fn settled(&mut self, media_uri: &str) {
        self.wanted.retain(|(_, wanted)| wanted != media_uri);
    }
}

impl Actor {
    /// The most the copies only the cache owns may take, in bytes; 0 while the cache is off.
    fn stream_cache_limit(&self) -> u64 {
        self.settings.load().ui.stream_cache_limit()
    }

    /// Follows playback: the tracks after the playing one that have no copy to play are
    /// fetched in their order, and a fetch for a track that is no longer near stops.
    pub(super) async fn play_ahead(
        &mut self,
        playing: Option<TrackKey>,
        upcoming: Vec<TrackKey>,
        active: &mut Vec<ActiveDownload>,
    ) {
        let tracks = playing.iter().chain(&upcoming).copied().collect::<Vec<_>>();
        let uris = match self.catalog.track_media_uris(&tracks).await {
            Ok(uris) => uris.into_iter().collect::<HashMap<_, _>>(),
            Err(error) => {
                warn!(%error, "could not read the tracks the stream cache follows");
                return;
            }
        };
        let playing = playing.and_then(|track| uris.get(&track).cloned());
        let upcoming = (upcoming.iter())
            .filter_map(|track| uris.get(track).cloned())
            .collect::<Vec<_>>();
        let skips_local = self.settings.load().ui.playback.stream_cache_skips_local;
        let quality = self.settings.stream_quality();
        let mut wanted = VecDeque::new();
        let mut local = HashMap::<SourceId, bool>::new();
        if self.stream_cache_limit() > 0 {
            for media_uri in &upcoming {
                let Some(source_id) = media_source_id(&self.catalog, media_uri).await else {
                    continue;
                };
                if !self.attached.contains_key(&source_id) {
                    continue;
                }
                if skips_local {
                    let on_local = match local.get(&source_id) {
                        Some(on_local) => *on_local,
                        None => {
                            let on_local = (self.source(&source_id).await)
                                .is_some_and(|source| source.on_local_address());
                            local.insert(source_id.clone(), on_local);
                            on_local
                        }
                    };
                    if on_local {
                        continue;
                    }
                }
                if !self.has_copy(media_uri, quality).await {
                    wanted.push_back((source_id, media_uri.clone()));
                }
            }
        }
        debug!(
            upcoming = upcoming.len(),
            wanted = wanted.len(),
            "the stream cache follows playback"
        );
        self.ahead.near = playing.iter().chain(&upcoming).cloned().collect();
        self.ahead.wanted = wanted;
        self.ahead.waiting = false;
        let near = self.ahead.near.clone();
        self.abort_matching(active, false, |download| {
            download.subject == DownloadSubject::StreamCache && !near.contains(&download.media_uri)
        })
        .await;
        if let Some(playing) = playing {
            self.mark_played(&playing).await;
        }
        self.evict_stream_cache(active).await;
    }

    /// Whether playback plays the track from this device at `quality`: from its own or its
    /// mapped file, a download, or a copy of the cache that is the track's own audio or the
    /// encoding its stream would be. A track that is gone has nothing to fetch.
    async fn has_copy(&self, media_uri: &str, quality: AudioQuality) -> bool {
        let Ok(playable) = playable(&self.catalog, media_uri).await else {
            return true;
        };
        let streamed = library::media_encoding_key(playable.stream_encoding(quality));
        let found = tokio::task::spawn_blocking(move || {
            let mapped = playable.mapped.as_ref();
            playable.local_uri().is_some()
                || mapped
                    .and_then(library::MappedFile::locate_playable)
                    .is_some()
                || playable.download.as_ref().is_some_and(|download| {
                    (download.cached.as_deref())
                        .is_none_or(|kept| kept == "original" || kept == streamed)
                        && library::file_media_path(&download.uri).is_none_or(|path| path.is_file())
                })
        });
        found.await.unwrap_or(true)
    }

    /// The next track the cache fetches: one at a time, so the nearest lands first.
    pub(super) fn peek_ahead(
        &self,
        active: &[ActiveDownload],
        waiting: &HashSet<String>,
    ) -> Option<(SourceId, String)> {
        let fetching =
            (active.iter()).any(|download| download.subject == DownloadSubject::StreamCache);
        if self.ahead.waiting || fetching {
            return None;
        }
        (self.ahead.wanted.iter())
            .find(|(source_id, media_uri)| {
                self.attached.contains_key(source_id) && !waiting.contains(media_uri)
            })
            .cloned()
    }

    /// A track of the cache cannot have a copy.
    pub(super) fn settle_ahead(&mut self, media_uri: &str) {
        self.ahead.settled(media_uri);
    }

    /// The cache has a copy of a track now. Playback hears of it, since a copy only the
    /// cache owns changes nothing the Catalog shows.
    pub(super) fn kept_ahead(&mut self, media_uri: &str) {
        self.ahead.settled(media_uri);
        if let Some(kept) = &self.ahead.kept {
            let _ = kept.try_send(media_uri.to_owned());
        }
    }

    pub(super) async fn finish_ahead(
        &mut self,
        download: ActiveDownload,
        joined: Result<Result<DownloadAudio, DownloadFailure>, tokio::task::JoinError>,
        active: &mut Vec<ActiveDownload>,
    ) {
        let ActiveDownload {
            source_id,
            media_uri,
            subject,
            mut paths,
            quality,
            ..
        } = download;
        let failure = match joined {
            Ok(Ok(audio)) => {
                paths.audio = audio;
                self.commit_transfer(&source_id, &media_uri, &subject, &paths, quality)
                    .await
                    .err()
            }
            Ok(Err(failure)) => Some(failure),
            Err(error) => Some(DownloadFailure::Item(error.to_string())),
        };
        match failure {
            None => self.kept_ahead(&media_uri),
            // The partial file stays for the next try.
            Some(DownloadFailure::Retry(error)) => {
                debug!(%error, %media_uri, "the stream cache waits for the server");
                self.ahead.waiting = true;
            }
            Some(DownloadFailure::Item(error) | DownloadFailure::NeedsAttention(error)) => {
                debug!(%error, %media_uri, "the stream cache left a track out");
                self.ahead.settled(&media_uri);
                if let Err(error) = discard_staging(&paths).await {
                    warn!(%error, %media_uri, "could not remove a stream cache transfer");
                }
            }
        }
        self.evict_stream_cache(active).await;
    }

    /// Playing a copy of the cache makes it the last one the cache gives up.
    async fn mark_played(&self, media_uri: &str) {
        let Ok(Some(file)) = self.catalog.download_file(media_uri).await else {
            return;
        };
        if file.owners != library::STREAM_CACHE_OWNERS {
            return;
        }
        let Some(path) = (completed_download_paths(&self.root, &file).ok())
            .and_then(|paths| paths.audio.native().map(Path::to_owned))
        else {
            return;
        };
        let marked = tokio::task::spawn_blocking(move || {
            std::fs::File::options()
                .append(true)
                .open(path)?
                .set_modified(SystemTime::now())
        })
        .await;
        if let Ok(Err(error)) = marked {
            debug!(%error, %media_uri, "could not mark a stream cache copy as played");
        }
    }

    /// Gives up the copies played longest ago until the cache fits its limit. The playing
    /// track and the ones after it stay; once they alone fill the cache, nothing more is
    /// fetched. A cache that is off gives up everything.
    pub(super) async fn evict_stream_cache(&mut self, active: &mut Vec<ActiveDownload>) {
        let limit = self.stream_cache_limit();
        if limit == 0 {
            self.ahead.wanted.clear();
            self.abort_matching(active, false, |download| {
                download.subject == DownloadSubject::StreamCache
            })
            .await;
        }
        let files = match self.catalog.stream_cache_files().await {
            Ok(files) => files,
            Err(error) => {
                warn!(%error, "could not read the stream cache");
                return;
            }
        };
        let size = |file: &library::DownloadFile| {
            u64::try_from(file.completed_size.unwrap_or_default()).unwrap_or_default()
        };
        let mut total = files.iter().map(size).sum::<u64>();
        if total > limit {
            let root = Arc::clone(&self.root);
            let played = tokio::task::spawn_blocking(move || {
                let mut played = (files.into_iter())
                    .map(|file| {
                        let last = (completed_download_paths(&root, &file).ok())
                            .and_then(|paths| paths.audio.native().map(Path::to_owned))
                            .and_then(|path| std::fs::metadata(path).ok())
                            .and_then(|metadata| metadata.modified().ok());
                        (last, file)
                    })
                    .collect::<Vec<_>>();
                played.sort_by_key(|(last, _)| *last);
                played
            })
            .await
            .unwrap_or_default();
            for (_, mut file) in played {
                if total <= limit {
                    break;
                }
                if limit > 0 && self.ahead.near.contains(&file.media_uri) {
                    continue;
                }
                let freed = size(&file);
                match self.store_owners(&mut file, HashSet::new()).await {
                    Ok(()) => total -= freed,
                    Err(error) => {
                        warn!(%error, media_uri=%file.media_uri, "could not remove a stream cache copy");
                    }
                }
            }
        }
        if total >= limit {
            self.ahead.wanted.clear();
        }
    }

    /// Removes every copy only the cache owns.
    pub(super) async fn clear_stream_cache(
        &mut self,
        active: &mut Vec<ActiveDownload>,
    ) -> Result<(), String> {
        self.ahead.wanted.clear();
        self.abort_matching(active, false, |download| {
            download.subject == DownloadSubject::StreamCache
        })
        .await;
        let files = (self.catalog.stream_cache_files().await).map_err(|error| error.to_string())?;
        let mut failure = Ok(());
        for mut file in files {
            if let Err(error) = self.store_owners(&mut file, HashSet::new()).await {
                failure = Err(error);
            }
        }
        failure
    }
}
