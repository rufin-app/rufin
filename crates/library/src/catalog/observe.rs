//! A source's tracks as another installation observes them, read by key.
use crate::catalog::views::{Art, Owner};
use crate::{Catalog, Freshness, LibraryResult, PeerTrack, SourceId, TrackObservation};

const SOURCE_KEY: &str = "(SELECT source_key FROM sources WHERE object_id=?1)";

#[derive(sqlx::FromRow)]
struct ObservedTrack {
    track_key: i64,
    object_id: String,
    album_object_id: Option<String>,
    metadata: String,
    source_path: Option<String>,
    cue_path: Option<String>,
    cue_start_millis: Option<i64>,
    cue_end_millis: Option<i64>,
    first_seen_at: Option<i64>,
    audio_revision: Option<Vec<u8>>,
    audio_hash: Option<Vec<u8>>,
    source_lyrics: Option<String>,
}

impl Catalog {
    /// Changes when a track of the source is added, removed or its file is rewritten, and
    /// when source lyrics change, as a sidecar edit does without touching the audio file.
    pub async fn source_marker(&self, source: &SourceId) -> LibraryResult<Freshness> {
        let mut connection = self.acquire_reader().await?;
        let (tracks, modified, last, lyrics): (i64, Option<i64>, Option<i64>, Option<i64>) =
            sqlx::query_as(sqlx::AssertSqlSafe(format!(
                "SELECT (SELECT count(*) FROM tracks WHERE source_key={SOURCE_KEY} AND indexed=1),
                    (SELECT max(mtime_ns) FROM files WHERE source_key={SOURCE_KEY}),
                    (SELECT max(track_key) FROM tracks WHERE source_key={SOURCE_KEY} AND indexed=1),
                    (SELECT lyrics.updated_at FROM lyrics_cache lyrics JOIN tracks track ON track.media_uri=lyrics.media_uri
                     WHERE lyrics.authority='source' AND track.source_key={SOURCE_KEY} ORDER BY lyrics.updated_at DESC LIMIT 1)"
            )))
            .bind(source.as_str())
            .fetch_one(&mut *connection)
            .await?;
        Freshness::new(
            [
                tracks,
                modified.unwrap_or_default(),
                last.unwrap_or_default(),
                lyrics.unwrap_or_default(),
            ]
            .map(i64::to_be_bytes)
            .concat(),
        )
    }

    /// The object ids of the source's indexed tracks.
    pub async fn source_track_ids(&self, source: &SourceId) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT object_id FROM tracks WHERE source_key={SOURCE_KEY} AND indexed=1 AND object_id IS NOT NULL ORDER BY object_id"
        )))
        .bind(source.as_str())
        .fetch_all(&mut *connection)
        .await?)
    }

    /// The source's indexed tracks whose file, CUE sheet, a picture in their directory or one
    /// above it, or source lyrics changed after `since`, in unix seconds.
    pub async fn source_changed_since(
        &self,
        source: &SourceId,
        since: i64,
    ) -> LibraryResult<Vec<String>> {
        let mut connection = self.acquire_reader().await?;
        Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT track.object_id FROM tracks track JOIN track_details details USING(track_key)
             JOIN files file ON file.source_key=track.source_key AND file.path IN(details.source_path,details.cue_path)
             WHERE track.source_key={SOURCE_KEY} AND track.indexed=1 AND track.object_id IS NOT NULL AND file.mtime_ns>?2
             UNION
             SELECT track.object_id FROM (SELECT DISTINCT root,rtrim(relative_path,replace(relative_path,'/','')) directory FROM files
                 WHERE source_key={SOURCE_KEY} AND kind='image' AND mtime_ns>?2) image
             JOIN files file ON file.source_key={SOURCE_KEY} AND file.kind='media' AND file.root=image.root
                 AND substr(file.relative_path,1,length(image.directory))=image.directory
             JOIN track_details details ON details.source_path=file.path JOIN tracks track USING(track_key)
             WHERE track.indexed=1 AND track.object_id IS NOT NULL
             UNION
             SELECT track.object_id FROM lyrics_cache lyrics JOIN tracks track ON track.media_uri=lyrics.media_uri
             WHERE lyrics.authority='source' AND lyrics.updated_at>?3 AND track.source_key={SOURCE_KEY}
               AND track.indexed=1 AND track.object_id IS NOT NULL"
        )))
        .bind(source.as_str())
        .bind(since.saturating_mul(1_000_000_000))
        .bind(since)
        .fetch_all(&mut *connection)
        .await?)
    }

    /// The source's indexed tracks of `ids` as observations: object ids, metadata,
    /// root-relative paths, the CUE window, audio keys, the picture shown here and source
    /// lyrics. They name no media URI or user state.
    pub async fn source_observations(
        &self,
        source: &SourceId,
        ids: &[String],
    ) -> LibraryResult<Vec<PeerTrack>> {
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<ObservedTrack> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT track.track_key,track.object_id,album.object_id album_object_id,{} metadata,
                file.relative_path source_path,cue.relative_path cue_path,details.cue_start_millis,details.cue_end_millis,
                track.first_seen_at,details.audio_revision,details.audio_hash,
                (SELECT lyrics FROM lyrics_cache WHERE media_uri=track.media_uri AND authority='source' AND role='original' AND language='' AND script='') source_lyrics
             FROM json_each(?2) requested
             JOIN tracks track ON track.source_key={SOURCE_KEY} AND track.object_id=requested.value AND track.indexed=1
             LEFT JOIN track_details details USING(track_key)
             LEFT JOIN albums album ON album.album_key=track.album_key
             LEFT JOIN files file ON file.source_key=track.source_key AND file.path=details.source_path
             LEFT JOIN files cue ON cue.source_key=track.source_key AND cue.path=details.cue_path
             ORDER BY requested.key",
            crate::track_metadata_sql("track", "details")
        )))
        .bind(source.as_str())
        .bind(serde_json::to_string(ids)?)
        .fetch_all(&mut *connection)
        .await?;
        // Only pictures from the source's files: a cover or artist lookup is the observer's
        // own, and so is the choice between a track's cover and its album's.
        let (own, album): (Vec<_>, Vec<_>) = self
            .file_arts(rows.iter().map(|row| row.track_key))
            .into_iter()
            .unzip();
        let own = crate::catalog::fetch::bindings(&mut connection, &own).await?;
        let album = crate::catalog::fetch::bindings(&mut connection, &album).await?;
        let hash = |bytes: Option<Vec<u8>>| bytes.and_then(|bytes| bytes.try_into().ok());
        rows.into_iter()
            .zip(own.into_iter().zip(album))
            .map(|(row, (artwork_binding, album_artwork_binding))| {
                Ok(PeerTrack {
                    track: TrackObservation {
                        object_id: row.object_id,
                        album_object_id: row.album_object_id,
                        metadata: serde_json::from_str(&row.metadata)?,
                        source_path: row.source_path,
                        cue_path: row.cue_path,
                        cue_start_millis: row.cue_start_millis,
                        cue_end_millis: row.cue_end_millis,
                        first_seen_at: row.first_seen_at,
                        audio_revision: hash(row.audio_revision),
                        audio_hash: hash(row.audio_hash),
                        source_lyrics: row.source_lyrics,
                        artwork_binding,
                        ..Default::default()
                    },
                    album_artwork_binding,
                })
            })
            .collect()
    }

    /// The picture `source_observations` names for a track of the source, or for an album.
    pub async fn source_picture(
        &self,
        source: &SourceId,
        album: bool,
        object: &str,
    ) -> LibraryResult<Option<Vec<u8>>> {
        let mut connection = self.acquire_reader().await?;
        let sql = if album {
            format!("SELECT album_key FROM albums WHERE source_key={SOURCE_KEY} AND object_id=?2")
        } else {
            format!(
                "SELECT track_key FROM tracks WHERE source_key={SOURCE_KEY} AND object_id=?2 AND indexed=1"
            )
        };
        let Some(key) = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(sql))
            .bind(source.as_str())
            .bind(object)
            .fetch_optional(&mut *connection)
            .await?
        else {
            return Ok(None);
        };
        let art = if album {
            match self.arts([Some(Owner::Album(key))])[0] {
                art @ Art::Album(_) => art,
                _ => Art::None,
            }
        } else {
            self.file_arts([key])[0].0
        };
        Ok(crate::catalog::fetch::bindings(&mut connection, &[art])
            .await?
            .pop()
            .flatten())
    }

    /// A page of the source's indexed tracks that came from files, after track key `after`:
    /// each by its key as its scan observed it, with the tags it keeps, its own picture and no
    /// user state, so that writing one back changes only what the writer changed; then the
    /// paths of its file and of its CUE sheet under their roots.
    pub async fn file_track_page(
        &self,
        source: crate::SourceKey,
        after: i64,
    ) -> LibraryResult<Vec<(i64, TrackObservation, [Option<String>; 2])>> {
        #[derive(sqlx::FromRow)]
        struct FileTrack {
            track_key: i64,
            object_id: String,
            media_uri: String,
            album_object_id: Option<String>,
            metadata: String,
            source_path: Option<String>,
            cue_path: Option<String>,
            cue_start_millis: Option<i64>,
            cue_end_millis: Option<i64>,
            audio_revision: Option<Vec<u8>>,
            native_artwork: Option<Vec<u8>>,
            relative_path: Option<String>,
            cue_relative_path: Option<String>,
        }
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<FileTrack> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT track.track_key,track.object_id,track.media_uri,album.object_id album_object_id,{} metadata,
                details.source_path,details.cue_path,details.cue_start_millis,details.cue_end_millis,details.audio_revision,details.native_artwork,
                file.relative_path,cue.relative_path cue_relative_path
             FROM tracks track JOIN track_details details USING(track_key)
             LEFT JOIN albums album ON album.album_key=track.album_key
             LEFT JOIN files file ON file.source_key=track.source_key AND file.path=details.source_path
             LEFT JOIN files cue ON cue.source_key=track.source_key AND cue.path=details.cue_path
             WHERE track.source_key=?1 AND track.track_key>?2 AND track.indexed=1 AND track.object_id IS NOT NULL
                AND (details.cue_path IS NOT NULL OR json_type(details.metadata,'$.tags') IS NOT NULL)
             ORDER BY track.track_key LIMIT 500",
            crate::track_metadata_sql("track", "details")
        )))
        .bind(source)
        .bind(after)
        .fetch_all(&mut *connection)
        .await?;
        rows.into_iter()
            .map(|row| {
                let mut metadata: serde_json::Value = serde_json::from_str(&row.metadata)?;
                let tags = match metadata.get_mut("tags").map(serde_json::Value::take) {
                    Some(tags) => serde_json::from_value(tags)?,
                    None => Default::default(),
                };
                Ok((
                    row.track_key,
                    TrackObservation {
                        object_id: row.object_id,
                        media_uri: Some(row.media_uri),
                        album_object_id: row.album_object_id,
                        metadata: serde_json::from_value(metadata)?,
                        tags,
                        source_path: row.source_path,
                        cue_path: row.cue_path,
                        cue_start_millis: row.cue_start_millis,
                        cue_end_millis: row.cue_end_millis,
                        audio_revision: row
                            .audio_revision
                            .and_then(|revision| revision.try_into().ok()),
                        artwork_binding: row.native_artwork,
                        ..Default::default()
                    },
                    [row.relative_path, row.cue_relative_path],
                ))
            })
            .collect()
    }

    /// The URIs of source tracks by source id and object id, in one read. A pair without a
    /// track is left out.
    pub async fn track_uris(
        &self,
        objects: &[(SourceId, String)],
    ) -> LibraryResult<Vec<(SourceId, String, String)>> {
        if objects.is_empty() {
            return Ok(Vec::new());
        }
        let requested = objects
            .iter()
            .map(|(source, object)| (source.as_str(), object.as_str()))
            .collect::<Vec<_>>();
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT source.object_id,track.object_id,track.media_uri FROM json_each(?1) requested
             JOIN sources source ON source.object_id=requested.value->>0
             JOIN tracks track ON track.source_key=source.source_key
                AND track.object_id=requested.value->>1",
        )
        .bind(serde_json::to_string(&requested)?)
        .fetch_all(&mut *connection)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(source, object, uri)| (SourceId::new(source), object, uri))
            .collect())
    }

    /// The source id and object id of each track of `uris` that has both, by URI.
    pub async fn track_objects(
        &self,
        uris: &[String],
    ) -> LibraryResult<Vec<(String, SourceId, String)>> {
        let mut connection = self.acquire_reader().await?;
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT track.media_uri,source.object_id,track.object_id FROM json_each(?1) requested
             JOIN tracks track ON track.media_uri=requested.value JOIN sources source USING(source_key)
             WHERE track.object_id IS NOT NULL",
        )
        .bind(serde_json::to_string(uris)?)
        .fetch_all(&mut *connection)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(uri, source, object)| (uri, SourceId::new(source), object))
            .collect())
    }
}
