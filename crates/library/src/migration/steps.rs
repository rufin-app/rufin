//! The upgrade of a Store a release wrote: one step per schema version since 0.17's 60, run in
//! place in one transaction. A branch that changes the schema raises `STORE_VERSION` and adds
//! the step from the version before it. A step keeps its own text of what it creates, since
//! `STORE_SCHEMA` moves on.

use std::path::Path;

use sqlx::{Connection, SqliteConnection};

use crate::schema::{STORE_SCHEMA, STORE_VERSION};
use crate::{LibraryError, LibraryResult};

/// The schema 0.17 released, where the steps start.
const RELEASED: i64 = 60;

/// Upgrades the Store at `path` when a release since 0.17 wrote it, and says whether its
/// albums resolve again; `None` when it is new or already this version's. Any other Store
/// fails, and so does one the steps leave unlike a new Store: the caller rebuilds both.
pub(crate) async fn upgrade(path: &Path) -> LibraryResult<Option<bool>> {
    let mut connection = crate::db::open_writer(path).await?;
    let upgraded = async {
        let version = crate::schema::pragma(&mut connection, "user_version").await?;
        match version {
            0 | STORE_VERSION => return Ok(None),
            RELEASED..STORE_VERSION => {}
            version => {
                return Err(LibraryError::Migration(format!(
                    "the Store has schema {version}, this version has {STORE_VERSION}"
                )));
            }
        }
        let mut transaction = connection.begin().await?;
        if version < 61 {
            to_61(&mut *transaction).await?;
        }
        if version < 62 {
            to_62(&mut *transaction).await?;
        }
        if version < 63 {
            to_63(&mut *transaction).await?;
        }
        if version < 64 {
            to_64(&mut *transaction).await?;
        }
        if version < 65 {
            to_65(&mut *transaction).await?;
        }
        if version < 66 {
            to_66(&mut *transaction).await?;
        }
        if version < 67 {
            to_67(&mut *transaction).await?;
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "PRAGMA user_version={STORE_VERSION}"
        )))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        tracing::info!(from = version, to = STORE_VERSION, "upgraded the Store");
        // A Store whose old shape was not the release's keeps what the steps carried.
        if !is_new_shape(&mut connection).await? {
            return Err(LibraryError::Migration(
                "the upgraded Store has other tables than a new one".into(),
            ));
        }
        // 63 classifies albums by their release types anew.
        Ok(Some(version < 63))
    }
    .await;
    connection.close().await?;
    upgraded
}

/// Marks a Store the Catalog could not load for a rebuild on the next start, when the failure
/// is the Store's: it is damaged, or lacks this version's tables or columns. A failed read the
/// Store's shape cannot explain is left alone, so it does not rebuild the Store on every start.
pub(crate) async fn condemn(
    connection: &mut SqliteConnection,
    error: &LibraryError,
) -> LibraryResult<bool> {
    let unusable = match error {
        LibraryError::Damaged(_) => true,
        LibraryError::InvalidStore(_) => !is_new_shape(connection).await?,
        _ => false,
    };
    if unusable {
        sqlx::query("PRAGMA user_version=-1")
            .execute(connection)
            .await?;
    }
    Ok(unusable)
}

/// Whether the Store has a new Store's tables, columns in their order, and indexes.
async fn is_new_shape(connection: &mut SqliteConnection) -> LibraryResult<bool> {
    type Shape = Vec<(String, String, String, i64, Option<String>, i64)>;
    const COLUMNS: &str = "SELECT s.name,c.name,c.type,c.\"notnull\",c.dflt_value,c.pk
        FROM sqlite_schema s JOIN pragma_table_info(s.name) c
        WHERE s.type='table' AND s.name NOT LIKE 'sqlite_%' ORDER BY s.name,c.cid";
    const INDEXES: &str = "SELECT tbl_name,name FROM sqlite_schema
        WHERE type<>'table' AND sql IS NOT NULL ORDER BY tbl_name,name";
    let mut new = SqliteConnection::connect("sqlite::memory:").await?;
    sqlx::raw_sql(STORE_SCHEMA).execute(&mut new).await?;
    let columns: Shape = sqlx::query_as(COLUMNS).fetch_all(&mut new).await?;
    let indexes: Vec<(String, String)> = sqlx::query_as(INDEXES).fetch_all(&mut new).await?;
    new.close().await?;
    // The lyrics search tables are not in the schema text.
    let named = |table: &String| columns.iter().any(|column| column.0 == *table);
    let mut stored: Shape = sqlx::query_as(COLUMNS).fetch_all(&mut *connection).await?;
    stored.retain(|column| named(&column.0));
    let mut stored_indexes: Vec<(String, String)> =
        sqlx::query_as(INDEXES).fetch_all(connection).await?;
    stored_indexes.retain(|index| named(&index.0));
    Ok(stored == columns && stored_indexes == indexes)
}

/// 0.17 kept its counts from outside Rufin by source and object id, and the 0.16 baselines of
/// file-library tracks as native counts, which those tracks never show. Both become play
/// counts.
async fn to_61(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(
        "CREATE TABLE play_counts (
            media_uri TEXT NOT NULL CHECK (media_uri <> ''),
            period TEXT NOT NULL,
            plays INTEGER NOT NULL CHECK (plays >= 0),
            skips INTEGER NOT NULL DEFAULT 0 CHECK (skips >= 0),
            last_played_at INTEGER,
            PRIMARY KEY(media_uri, period)
        ) STRICT, WITHOUT ROWID;",
    )
    .execute(&mut *connection)
    .await?;
    let counts = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{} FROM legacy_activity",
        crate::activity::RELEASED_COUNTS
    )))
    .fetch_all(&mut *connection)
    .await?;
    crate::activity::carry_released_counts(&mut *connection, counts).await?;
    let files = "source_key IN(SELECT source_key FROM sources WHERE kind IN('local','webdav','smb')) AND native_play_count IS NOT NULL";
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "INSERT INTO play_counts(media_uri,period,plays,skips,last_played_at)
            SELECT media_uri,'lifetime',native_play_count,0,native_last_played FROM tracks WHERE {files} {};
         UPDATE tracks SET native_play_count=NULL,native_last_played=NULL WHERE {files};
         DROP TABLE legacy_activity;",
        crate::activity::ADD_PLAY_COUNTS
    )))
    .execute(connection)
    .await?;
    Ok(())
}

/// 0.17 named a playlist's one folder in its row, and a folder held one kind.
async fn to_62(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(
        "CREATE TABLE playlist_folder_playlists (
            playlist_key INTEGER NOT NULL REFERENCES playlists ON DELETE CASCADE,
            folder_key INTEGER NOT NULL REFERENCES playlist_folders ON DELETE CASCADE,
            PRIMARY KEY(playlist_key, folder_key)
        ) STRICT, WITHOUT ROWID;
        CREATE INDEX playlist_folder_playlists_folder_idx ON playlist_folder_playlists(folder_key);
        CREATE TABLE playlist_folder_smart_playlists (
            smart_playlist_key INTEGER NOT NULL REFERENCES smart_playlists ON DELETE CASCADE,
            folder_key INTEGER NOT NULL REFERENCES playlist_folders ON DELETE CASCADE,
            PRIMARY KEY(smart_playlist_key, folder_key)
        ) STRICT, WITHOUT ROWID;
        CREATE INDEX playlist_folder_smart_playlists_folder_idx ON playlist_folder_smart_playlists(folder_key);
        INSERT INTO playlist_folder_playlists(playlist_key,folder_key)
            SELECT playlist_key,playlist_folder_key FROM playlists
            WHERE playlist_folder_key IN(SELECT folder_key FROM playlist_folders);
        INSERT INTO playlist_folder_smart_playlists(smart_playlist_key,folder_key)
            SELECT smart_playlist_key,playlist_folder_key FROM smart_playlists
            WHERE playlist_folder_key IN(SELECT folder_key FROM playlist_folders);
        DROP INDEX playlists_folder_idx;
        DROP INDEX smart_playlists_folder_idx;
        ALTER TABLE playlists DROP COLUMN playlist_folder_key;
        ALTER TABLE smart_playlists DROP COLUMN playlist_folder_key;
        ALTER TABLE playlist_folders DROP COLUMN smart;",
    )
    .execute(connection)
    .await?;
    Ok(())
}

/// 0.17 kept MusicBrainz's release types per album; they are kept per release identity, and
/// an album asks for them only when no source typed it.
async fn to_63(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(
        "CREATE TABLE musicbrainz_release_types (
            identity TEXT PRIMARY KEY,
            release_types BLOB NOT NULL
        ) STRICT, WITHOUT ROWID;
        INSERT OR IGNORE INTO musicbrainz_release_types(identity,release_types)
            SELECT lookup_identity,release_types FROM album_release_observations;
        DROP TABLE album_release_observations;
        ALTER TABLE albums DROP COLUMN release_lookup_identity;
        CREATE INDEX albums_release_identity_idx ON albums(release_identity) WHERE release_identity IS NOT NULL;",
    )
    .execute(connection)
    .await?;
    Ok(())
}

/// Radio stations: a server's, and the ones Rufin keeps.
async fn to_64(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(
        "CREATE TABLE stations (
            station_key INTEGER PRIMARY KEY,
            -- The server that keeps the station; NULL when Rufin does.
            source_key INTEGER REFERENCES sources ON DELETE CASCADE,
            object_id TEXT NOT NULL CHECK (object_id <> ''),
            name TEXT NOT NULL,
            stream_url TEXT NOT NULL CHECK (stream_url <> ''),
            homepage TEXT NOT NULL DEFAULT '',
            cover TEXT,
            -- The address of its picture, where the search that found it gave one.
            logo TEXT,
            -- The picture a person chose for it.
            artwork_bytes BLOB,
            artwork_mime TEXT,
            artwork_revision TEXT,
            -- Its place among the stations of its source, as a person ordered them.
            position INTEGER NOT NULL DEFAULT 0
        ) STRICT;
        CREATE INDEX stations_source_idx ON stations(source_key, object_id);",
    )
    .execute(connection)
    .await?;
    Ok(())
}

/// Picked lyrics are found without reading the lyrics cache.
async fn to_65(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(
        "CREATE INDEX lyrics_cache_picked_idx ON lyrics_cache(media_uri, authority, role, language, script) WHERE picked;",
    )
    .execute(connection)
    .await?;
    Ok(())
}

/// A peer source is listed whole again: its owner now tells each track's own picture and its
/// album's, which no change since the last sync names.
async fn to_66(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql("UPDATE sources SET synced_at=NULL, freshness=NULL WHERE kind='peer';")
        .execute(connection)
        .await?;
    Ok(())
}

/// Lists sort by the audio properties, so they move from the details JSON to typed columns
/// the Catalog holds; the file size stays in the JSON. Tracks take their file's modified time:
/// SMB and document listings carry it in their revisions, which the files take, and Navidrome
/// lists it again in a full sync. WebDAV gives it with its next listing.
async fn to_67(connection: &mut SqliteConnection) -> LibraryResult<()> {
    sqlx::raw_sql(
        "ALTER TABLE tracks ADD COLUMN bitrate_kbps INTEGER;
        ALTER TABLE tracks ADD COLUMN sample_rate_hz INTEGER;
        ALTER TABLE tracks ADD COLUMN bit_depth INTEGER;
        ALTER TABLE tracks ADD COLUMN channels INTEGER;
        UPDATE tracks SET (bitrate_kbps,sample_rate_hz,bit_depth,channels)=(
            SELECT json_extract(metadata,'$.audio_properties.bitrate_kbps'),
                json_extract(metadata,'$.audio_properties.sample_rate_hz'),
                json_extract(metadata,'$.audio_properties.bit_depth'),
                json_extract(metadata,'$.audio_properties.channels')
            FROM track_details WHERE track_details.track_key=tracks.track_key)
        WHERE track_key IN (SELECT track_key FROM track_details
            WHERE json_type(metadata,'$.audio_properties')='object');
        UPDATE track_details SET metadata=jsonb_remove(metadata,
            '$.audio_properties.bitrate_kbps','$.audio_properties.sample_rate_hz',
            '$.audio_properties.bit_depth','$.audio_properties.channels')
        WHERE json_type(metadata,'$.audio_properties')='object';
        UPDATE track_details SET metadata=jsonb_remove(metadata,'$.audio_properties')
        WHERE json(json_extract(metadata,'$.audio_properties'))='{}';
        ALTER TABLE tracks ADD COLUMN modified_at INTEGER;
        UPDATE files SET mtime_ns=(CAST(substr(revision,1,instr(revision,':')-1) AS INTEGER)
            -116444736000000000)*100
        WHERE mtime_ns=0 AND instr(revision,':')>0
            AND CAST(substr(revision,1,instr(revision,':')-1) AS INTEGER)>116444736000000000
            AND source_key IN (SELECT source_key FROM sources WHERE kind='smb');
        UPDATE files SET mtime_ns=CAST(substr(revision,1,instr(revision,':')-1) AS INTEGER)*1000000
        WHERE mtime_ns=0 AND path LIKE 'content:%' AND instr(revision,':')>0;
        UPDATE tracks SET modified_at=(SELECT file.mtime_ns/1000000000
            FROM track_details details JOIN files file ON file.source_key=tracks.source_key
                AND file.path=coalesce(details.cue_path,details.source_path)
            WHERE details.track_key=tracks.track_key AND file.mtime_ns>0);
        UPDATE sources SET synced_at=NULL, freshness=NULL WHERE kind='navidrome';",
    )
    .execute(connection)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_steps_alone_give_a_released_store_a_new_stores_shape() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("library.sqlite3");
        let mut released = crate::db::open_writer(&path).await.unwrap();
        sqlx::raw_sql(include_str!("../../tests/fixtures/store-0.17.1.sql"))
            .execute(&mut released)
            .await
            .unwrap();
        released.close().await.unwrap();
        // A Store the steps leave unlike a new one fails here, and is rebuilt.
        assert_eq!(upgrade(&path).await.unwrap(), Some(true));
        assert_eq!(upgrade(&path).await.unwrap(), None);
    }
}
