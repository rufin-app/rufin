//! What a link sends, kept in a file while it plays. The file fills as fast as the sender
//! gives, whatever the pace it is played at, so the song no longer hangs on the sender once
//! it has arrived. Nothing in the file can be read: it is sealed with a key that only this
//! process holds and that is gone with the stream, and the file has no name.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use chacha20poly1305::{ChaCha20Poly1305, KeyInit, Nonce, aead::Aead};
use futures_util::Stream;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;

const CHUNK: usize = 64 * 1024;

/// Each chunk is sealed under its place in the stream, so none stands for another.
fn nonce(chunk: u64) -> Nonce {
    let mut nonce = [0; 12];
    nonce[..8].copy_from_slice(&chunk.to_le_bytes());
    nonce.into()
}

fn sealing_failed() -> io::Error {
    io::Error::other("the buffered audio could not be sealed")
}

/// `reader`'s bytes, by way of a sealed file in `directory`. Dropping the stream stops the
/// reading and removes the file.
pub(crate) fn kept(
    mut reader: impl AsyncRead + Send + Unpin + 'static,
    directory: &Path,
) -> io::Result<impl Stream<Item = io::Result<Bytes>> + Send + 'static> {
    std::fs::create_dir_all(directory)?;
    let file = Arc::new(Mutex::new(tempfile::tempfile_in(directory)?));
    let mut key = [0_u8; 32];
    getrandom::fill(&mut key).map_err(io::Error::other)?;
    let cipher = ChaCha20Poly1305::new(&key.into());
    // The length of each sealed chunk, once the file holds it.
    let (sealed, ready) = mpsc::unbounded_channel::<io::Result<usize>>();
    let (writing, sealing) = (Arc::clone(&file), cipher.clone());
    tokio::spawn(async move {
        for chunk in 0.. {
            let mut plain = Vec::with_capacity(CHUNK);
            let filled = tokio::select! {
                _ = sealed.closed() => return,
                filled = async {
                    while plain.len() < CHUNK {
                        let room = (CHUNK - plain.len()) as u64;
                        if (&mut reader).take(room).read_buf(&mut plain).await? == 0 {
                            break;
                        }
                    }
                    io::Result::Ok(())
                } => filled,
            };
            if filled.is_ok() && plain.is_empty() {
                return;
            }
            let (file, cipher) = (Arc::clone(&writing), sealing.clone());
            let stored = match filled {
                Ok(()) => tokio::task::spawn_blocking(move || {
                    let bytes = (cipher.encrypt(&nonce(chunk), plain.as_slice()))
                        .map_err(|_| sealing_failed())?;
                    let mut file = file.lock().unwrap_or_else(|p| p.into_inner());
                    file.seek(SeekFrom::End(0))?;
                    file.write_all(&bytes)?;
                    Ok(bytes.len())
                })
                .await
                .unwrap_or_else(|error| Err(io::Error::other(error))),
                Err(error) => Err(error),
            };
            let failed = stored.is_err();
            if sealed.send(stored).is_err() || failed {
                return;
            }
        }
    });
    let state = (ready, file, cipher, 0_u64, 0_u64);
    Ok(futures_util::stream::try_unfold(
        state,
        |(mut ready, file, cipher, chunk, offset)| async move {
            let Some(length) = ready.recv().await.transpose()? else {
                return Ok(None);
            };
            let (reading, opening) = (Arc::clone(&file), cipher.clone());
            let plain = tokio::task::spawn_blocking(move || {
                let mut bytes = vec![0; length];
                {
                    let mut file = reading.lock().unwrap_or_else(|p| p.into_inner());
                    file.seek(SeekFrom::Start(offset))?;
                    file.read_exact(&mut bytes)?;
                }
                (opening.decrypt(&nonce(chunk), bytes.as_slice())).map_err(|_| sealing_failed())
            })
            .await
            .map_err(io::Error::other)??;
            let next = (ready, file, cipher, chunk + 1, offset + length as u64);
            Ok(Some((Bytes::from(plain), next)))
        },
    ))
}
