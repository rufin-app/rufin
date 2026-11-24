//! The peer source over the media protocol: this device answers for its own Local sources from
//! its Store, and asks the device that owns a peer source here.
use super::{ConnectOwner, Stream};
use crate::string_error;
use futures_util::future::BoxFuture;
use rufin_connect::media::{PeerByteMetadata, PeerByteResponse};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sources::{PeerAudio, SourceError, SourceId, SourceResult};
use std::{io::Read as _, pin::Pin, sync::Weak};
use tokio::io::{AsyncRead, AsyncReadExt};

/// A read of the catalog of one of the owner's Local sources. `Watch` is answered with a line
/// whenever the source's marker moved.
#[derive(Serialize, Deserialize)]
#[serde(tag = "read", rename_all = "snake_case")]
pub(super) enum Read {
    Marker,
    Ids,
    ChangedSince { since: i64 },
    Observations { ids: Vec<String> },
    Watch,
}

/// A source's changes are told once its writes pause this long, and this often while they
/// go on, so a scan is not answered commit by commit.
const WATCH_PAUSE: std::time::Duration = std::time::Duration::from_secs(2);
const WATCH_LIMIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Asks the device that owns a peer source.
pub(crate) struct Link(pub(crate) Weak<ConnectOwner>);

impl Link {
    async fn stream(&self, device: &str, request: Stream) -> SourceResult<PeerByteResponse> {
        let owner = self.0.upgrade().ok_or(SourceError::Cancelled)?;
        let network = owner
            .network
            .lock()
            .await
            .clone()
            .ok_or_else(|| SourceError::Network(localization::tr("Connect is not enabled")))?;
        let body = serde_json::to_vec(&request)?;
        let result = if request.carries_media() {
            network.fetch_media(device, body).await
        } else {
            network.stream_request(device, body).await
        };
        result.map_err(|failure| SourceError::Network(string_error(failure)))
    }

    async fn read<T: DeserializeOwned>(
        &self,
        device: &str,
        source: &SourceId,
        read: Read,
    ) -> SourceResult<T> {
        let mut response = self
            .stream(
                device,
                Stream::Catalog {
                    source: source.clone(),
                    read,
                },
            )
            .await?;
        let mut bytes = Vec::new();
        response
            .reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|failure| SourceError::Network(failure.to_string()))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}

impl sources::PeerLink for Link {
    fn marker<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
    ) -> BoxFuture<'a, SourceResult<library::Freshness>> {
        Box::pin(async move {
            let marker: Vec<u8> = self.read(device, source, Read::Marker).await?;
            Ok(library::Freshness::new(marker)?)
        })
    }

    fn ids<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
    ) -> BoxFuture<'a, SourceResult<(Vec<String>, i64)>> {
        Box::pin(self.read(device, source, Read::Ids))
    }

    fn changed_since<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
        since: i64,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        Box::pin(self.read(device, source, Read::ChangedSince { since }))
    }

    fn observations<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
        ids: &'a [String],
    ) -> BoxFuture<'a, SourceResult<Vec<library::PeerTrack>>> {
        Box::pin(self.read(device, source, Read::Observations { ids: ids.to_vec() }))
    }

    fn watch<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
    ) -> BoxFuture<'a, SourceResult<Pin<Box<dyn AsyncRead + Send + Unpin>>>> {
        Box::pin(async move {
            let request = Stream::Catalog {
                source: source.clone(),
                read: Read::Watch,
            };
            Ok(self.stream(device, request).await?.reader)
        })
    }

    fn image<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
        image_ref: &'a str,
        size: Option<u32>,
    ) -> BoxFuture<'a, SourceResult<sources::ImageBytes>> {
        Box::pin(async move {
            let mut response = self
                .stream(
                    device,
                    Stream::Image {
                        source: source.clone(),
                        image: image_ref.to_owned(),
                        size,
                    },
                )
                .await?;
            let mut bytes = Vec::new();
            response
                .reader
                .read_to_end(&mut bytes)
                .await
                .map_err(|failure| SourceError::Network(failure.to_string()))?;
            Ok(sources::ImageBytes {
                bytes,
                content_type: response.metadata["content_type"]
                    .as_str()
                    .map(str::to_owned),
            })
        })
    }

    fn open<'a>(
        &'a self,
        device: &'a str,
        source: &'a SourceId,
        object: &'a str,
        encoding: Option<library::AudioEncoding>,
        range: Option<(u64, Option<u64>)>,
        window: Option<(u64, Option<u64>)>,
    ) -> BoxFuture<'a, SourceResult<PeerAudio>> {
        Box::pin(async move {
            let response = self
                .stream(
                    device,
                    Stream::Audio {
                        source: source.clone(),
                        object: object.to_owned(),
                        encoding,
                        range,
                        window,
                    },
                )
                .await?;
            let metadata: PeerByteMetadata = serde_json::from_value(response.metadata)?;
            Ok(PeerAudio {
                reader: response.reader,
                length: metadata.length,
                offset: metadata.offset,
                content_type: metadata.content_type,
                validator: metadata.validator,
            })
        })
    }
}

impl ConnectOwner {
    /// Answers a request on the media protocol.
    pub(super) async fn answer_stream(
        &self,
        body: &[u8],
    ) -> Result<rufin_connect::media::StreamReply, String> {
        use rufin_connect::media::StreamReply;
        let request: Stream = serde_json::from_slice(body).map_err(string_error)?;
        let media = request.carries_media();
        let response = match request {
            Stream::Catalog { source, read } => {
                self.local_source(&source)?;
                let answer = match read {
                    Read::Watch => return self.watched(source).map(StreamReply::Control),
                    Read::Marker => serde_json::to_vec(
                        self.catalog
                            .source_marker(&source)
                            .await
                            .map_err(string_error)?
                            .as_bytes(),
                    ),
                    Read::Ids => {
                        // Taken before the read, so a change during it is still after the clock.
                        let clock = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |time| time.as_secs() as i64);
                        let ids = self
                            .catalog
                            .source_track_ids(&source)
                            .await
                            .map_err(string_error)?;
                        serde_json::to_vec(&(ids, clock))
                    }
                    Read::ChangedSince { since } => serde_json::to_vec(
                        &self
                            .catalog
                            .source_changed_since(&source, since)
                            .await
                            .map_err(string_error)?,
                    ),
                    Read::Observations { ids } => {
                        let mut tracks = self
                            .catalog
                            .source_observations(&source, &ids)
                            .await
                            .map_err(string_error)?;
                        // A picture is named by its row and changes with the local picture
                        // it is read from.
                        let name = |row: String, binding: &mut Option<Vec<u8>>| {
                            let Some(local) = binding else {
                                return Ok(());
                            };
                            let image = sources::NativeImageRef::new(
                                row,
                                Some(blake3::hash(local).to_hex().to_string()),
                            );
                            *binding = Some(
                                sources::native_artwork_binding(source.as_str(), &image)
                                    .map_err(string_error)?,
                            );
                            Ok::<_, String>(())
                        };
                        for row in &mut tracks {
                            let track = &mut row.track;
                            name(
                                format!("track:{}", track.object_id),
                                &mut track.artwork_binding,
                            )?;
                            if let Some(album) = &track.album_object_id {
                                name(format!("album:{album}"), &mut row.album_artwork_binding)?;
                            }
                        }
                        serde_json::to_vec(&tracks)
                    }
                }
                .map_err(string_error)?;
                Ok(PeerByteResponse {
                    metadata: serde_json::Value::Null,
                    reader: Box::pin(std::io::Cursor::new(answer)),
                })
            }
            Stream::Image {
                source,
                image,
                size,
            } => self.answer_image(&source, &image, size).await,
            Stream::Audio {
                source,
                object,
                encoding,
                range,
                window,
            } => {
                self.answer_audio(&source, &object, encoding, range, window)
                    .await
            }
            Stream::Observe => self.observed(),
        }?;
        Ok(if media {
            StreamReply::Media(response)
        } else {
            StreamReply::Control(response)
        })
    }

    /// A line whenever the marker of this device's source moved, until the reader closes the
    /// stream or Connect stops.
    fn watched(&self, source: SourceId) -> Result<PeerByteResponse, String> {
        use tokio::io::AsyncWriteExt;
        use tokio::sync::broadcast::error::RecvError;
        let session = self.active()?;
        let (mut lines, reader) = tokio::io::duplex(64);
        let mut changes = self.catalog.changes();
        let catalog = self.catalog.clone();
        self.runtime
            .spawn(session.stop.clone().run_until_cancelled_owned(async move {
                let marker = async || {
                    let marker = catalog.source_marker(&source).await.ok()?;
                    Some(marker.as_bytes().to_vec())
                };
                let mut told = marker().await;
                while !matches!(changes.recv().await, Err(RecvError::Closed)) {
                    let begun = std::time::Instant::now();
                    while begun.elapsed() < WATCH_LIMIT {
                        match tokio::time::timeout(WATCH_PAUSE, changes.recv()).await {
                            Err(_) => break,
                            Ok(Err(RecvError::Closed)) => return,
                            Ok(_) => {}
                        }
                    }
                    // An empty write still learns that the reader left.
                    let current = marker().await;
                    let line: &[u8] = if current == told { b"" } else { b"\n" };
                    if lines.write(line).await.is_err() {
                        return;
                    }
                    told = current;
                }
            }));
        Ok(PeerByteResponse {
            metadata: serde_json::Value::Null,
            reader: Box::pin(reader),
        })
    }

    /// Only this device's own Local sources are served.
    fn local_source(&self, source: &SourceId) -> Result<(), String> {
        self.settings
            .file()
            .load()
            .sources
            .configured
            .iter()
            .any(|item| item.configuration.source_id == *source && item.configuration.is_local())
            .then_some(())
            .ok_or_else(|| "This device does not hold the requested source".into())
    }

    /// A track's own picture, else its album's, or an album's own, from the source's files;
    /// no picture is an empty body.
    async fn answer_image(
        &self,
        source: &SourceId,
        image: &str,
        size: Option<u32>,
    ) -> Result<PeerByteResponse, String> {
        let (album, object) = match image.split_once(':') {
            Some(("track", object)) => (false, object),
            Some(("album", object)) => (true, object),
            _ => return Err("The requested image is unavailable on this device".into()),
        };
        self.local_source(source)?;
        let binding = self
            .catalog
            .source_picture(source, album, object)
            .await
            .map_err(string_error)?;
        let bytes = match binding {
            Some(binding) => {
                let binding = artwork::ArtworkBinding::opaque(&binding);
                let request = match size {
                    Some(size) => artwork::ArtworkRequest::new(binding, size),
                    None => artwork::ArtworkRequest::original(binding, 512),
                };
                self.source
                    .shared
                    .artwork
                    .image_bytes(request)
                    .await?
                    .unwrap_or_default()
            }
            None => Vec::new(),
        };
        Ok(PeerByteResponse {
            metadata: serde_json::json!({ "content_type": artwork::image_mime(&bytes) }),
            reader: Box::pin(std::io::Cursor::new(bytes)),
        })
    }

    /// The track's file: transcoded with the window asked for, or its original bytes in the
    /// range asked for.
    async fn answer_audio(
        &self,
        source: &SourceId,
        object: &str,
        encoding: Option<library::AudioEncoding>,
        range: Option<(u64, Option<u64>)>,
        window: Option<(u64, Option<u64>)>,
    ) -> Result<PeerByteResponse, String> {
        self.local_source(source)?;
        let location = self
            .catalog
            .playable_by_object(source, object)
            .await
            .map_err(string_error)?
            .and_then(|playable| playable.local_uri())
            .ok_or("The requested file is unavailable on this device")?;
        let path = library::file_media_path(&location);
        let mut metadata = PeerByteMetadata {
            length: None,
            offset: 0,
            content_type: encoding.map(|encoding| encoding.codec.content_type().into()),
            validator: None,
        };
        if let Some(encoding) = encoding {
            let mut stream = if path.is_some() {
                audio_processing::ResolvedStream::new(location)
            } else {
                sources::resolve_document_stream(&location)
                    .await
                    .map_err(string_error)?
            };
            if let Some((start, end)) = window {
                // An empty interval must never widen into the whole recording.
                if end.is_some_and(|end| end <= start) {
                    return Err(format!(
                        "requested audio interval {start}..{end:?} is empty"
                    ));
                }
                stream = stream.with_window(start, end.unwrap_or(u64::MAX / 1_000_000));
            }
            let reader = tokio::task::spawn_blocking(move || {
                audio_processing::TranscodedAudioReader::new(&stream, encoding.into(), || false)
                    .map_err(string_error)
            })
            .await
            .map_err(string_error)??;
            return Ok(PeerByteResponse {
                metadata: serde_json::to_value(metadata).map_err(string_error)?,
                reader: blocking_reader(reader),
            });
        }
        let start = range.map_or(0, |(start, _)| start);
        let end = move |length: u64| range.and_then(|(_, end)| end).unwrap_or(length).min(length);
        let reader: Pin<Box<dyn AsyncRead + Send + Unpin>> = match path {
            Some(path) => {
                let (file, reader) = rufin_connect::media::open_file(&path, start)
                    .await
                    .map_err(string_error)?;
                metadata.length = Some(file.length);
                metadata.validator = file.validator;
                Box::pin(reader.take(end(file.length).saturating_sub(start)))
            }
            None => {
                let (reader, length, validator) = tokio::task::spawn_blocking(move || {
                    use std::io::Seek;
                    let mut reader =
                        sources::open_document_input(&location).map_err(string_error)?;
                    let length = reader
                        .seek(std::io::SeekFrom::End(0))
                        .map_err(string_error)?;
                    let validator = sources::stat_document(&location).ok().and_then(|entry| {
                        entry.revision.map(|revision| {
                            blake3::hash(
                                format!("{location}:{}:{length}:{revision}", entry.native_id)
                                    .as_bytes(),
                            )
                            .to_hex()
                            .to_string()
                        })
                    });
                    reader
                        .seek(std::io::SeekFrom::Start(start))
                        .map_err(string_error)?;
                    Ok::<_, String>((
                        reader.take(end(length).saturating_sub(start)),
                        length,
                        validator,
                    ))
                })
                .await
                .map_err(string_error)??;
                metadata.length = Some(length);
                metadata.validator = validator;
                blocking_reader(reader)
            }
        };
        metadata.offset = start;
        Ok(PeerByteResponse {
            metadata: serde_json::to_value(metadata).map_err(string_error)?,
            reader,
        })
    }
}

/// A blocking reader served from a blocking thread.
fn blocking_reader(
    mut reader: impl std::io::Read + Send + 'static,
) -> Pin<Box<dyn AsyncRead + Send + Unpin>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    tokio::task::spawn_blocking(move || {
        let mut bytes = [0; 65536];
        loop {
            let chunk = match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(length) => Ok(std::io::Cursor::new(bytes[..length].to_vec())),
                Err(failure) => Err(failure),
            };
            let failed = chunk.is_err();
            if sender.blocking_send(chunk).is_err() || failed {
                break;
            }
        }
    });
    let stream = futures_util::stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|chunk| (chunk, receiver))
    });
    Box::pin(tokio_util::io::StreamReader::new(Box::pin(stream)))
}
