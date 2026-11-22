//! Delivers favorites and ratings to their servers while a configured server takes them, and
//! ratings into the files of a file source that writes them. A queued delivery wakes it, and
//! so does a refresh that may have made a source reachable. It retries deferred ones when they
//! are due.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use library::{Catalog, Change, Delivery, DeliveryKind, Scope, SourceKind, SourcePhase};
use sources::{RatingWrite, SourceEntityKind, SourceError, SourceId};
use tracing::info;

use super::{
    Shared, SourceOwner, configured_source, source_error_is_temporary, unix_seconds, writes_ratings,
};
use crate::features::{Feature, Pass};
use crate::runtime::SourceEvent;
use crate::settings::StoredSettings;

/// How long a server that could not take a delivery waits before the next try.
const RETRY_SECONDS: i64 = 30;
const PAGE: usize = 64;

fn keeps_copy(kind: SourceKind, delivery: &Delivery) -> bool {
    delivery.kind == DeliveryKind::Rating && kind.keeps_rating(delivery.value.unwrap_or(0))
}

pub(crate) struct Outbox {
    shared: Weak<Shared>,
    /// The pass that runs when the next deferred delivery is due.
    retry: Mutex<Option<tokio::task::AbortHandle>>,
    /// Held by the one pass that delivers, so an older value never lands after a newer one.
    delivering: tokio::sync::Mutex<()>,
}

impl Outbox {
    pub(super) fn new(shared: Weak<Shared>) -> Self {
        Self {
            shared,
            retry: Mutex::new(None),
            delivering: tokio::sync::Mutex::new(()),
        }
    }
}

impl Feature for Outbox {
    fn wanted(&self, settings: &StoredSettings, _: &Catalog) -> bool {
        settings.sources.configured.iter().any(|source| {
            writes_ratings(source)
                || source.configuration.is(|kind| {
                    kind.delivers(DeliveryKind::Favorite) || kind.delivers(DeliveryKind::Rating)
                })
        })
    }

    fn hears(&self, scope: &Scope) -> bool {
        matches!(
            scope,
            Scope::Held { .. }
                | Scope::Source {
                    phase: SourcePhase::Completed,
                    ..
                }
        )
    }

    /// Delivers what is due, then waits for the next due time.
    fn apply(self: Arc<Self>, _: Option<Vec<Arc<Change>>>) -> Pass {
        Box::pin(async move {
            let _delivering = self.delivering.lock().await;
            loop {
                let Some(owner) = self.shared.upgrade().map(|shared| SourceOwner { shared }) else {
                    return;
                };
                owner.shared.count_waiting_ratings().await;
                let next = match owner.shared.catalog.next_delivery_at().await {
                    Ok(next) => next,
                    Err(error) => {
                        owner.shared.warn_nonfatal(&error.to_string());
                        Some(unix_seconds().saturating_add(RETRY_SECONDS))
                    }
                };
                let delay = next.map(|next| next.saturating_sub(unix_seconds()).max(0) as u64);
                if delay == Some(0) {
                    owner.deliver_due().await;
                    continue;
                }
                let retry = delay.map(|delay| {
                    let outbox = Arc::clone(&self);
                    owner
                        .shared
                        .runtime
                        .spawn(async move {
                            tokio::time::sleep(Duration::from_secs(delay)).await;
                            outbox.apply(None).await;
                        })
                        .abort_handle()
                });
                let previous = std::mem::replace(
                    &mut *self.retry.lock().unwrap_or_else(|p| p.into_inner()),
                    retry,
                );
                if let Some(previous) = previous {
                    previous.abort();
                }
                return;
            }
        })
    }

    fn stopped(&self) {
        if let Some(retry) = self.retry.lock().unwrap_or_else(|p| p.into_inner()).take() {
            retry.abort();
        }
    }
}

impl SourceOwner {
    /// One pass over the deliveries that are due. A server that cannot take one now keeps its
    /// later deliveries waiting too.
    async fn deliver_due(&self) {
        let due = match self
            .shared
            .catalog
            .due_deliveries(unix_seconds(), PAGE)
            .await
        {
            Ok(due) => due,
            Err(error) => {
                self.shared.warn_nonfatal(&error.to_string());
                return;
            }
        };
        let mut unavailable = HashSet::new();
        for delivery in due {
            let Some(target) = self.delivery_target(&delivery).await else {
                self.drop_delivery(&delivery, None).await;
                continue;
            };
            let source_id = match &target {
                Target::Server(source_id, _, _) | Target::File(source_id, _) => source_id.clone(),
            };
            if unavailable.contains(&source_id) {
                continue;
            }
            let sent = match &target {
                Target::Server(_, entity, object_id) => {
                    self.send(&source_id, *entity, object_id, &delivery).await
                }
                Target::File(_, written) => self.write(&source_id, *written, &delivery).await,
            };
            match sent {
                Sent::Accepted { keep_copy } => {
                    info!(kind = ?delivery.kind, "Delivery accepted");
                    if let Err(error) = self.shared.catalog.delivered(&delivery, keep_copy).await {
                        self.shared.warn_nonfatal(&error.to_string());
                    }
                }
                Sent::Later => {
                    info!(kind = ?delivery.kind, "Deliveries deferred");
                    let at = unix_seconds().saturating_add(RETRY_SECONDS);
                    if let Err(error) = self.shared.catalog.defer_deliveries(&source_id, at).await {
                        self.shared.warn_nonfatal(&error.to_string());
                    }
                    unavailable.insert(source_id);
                }
                Sent::Refused => self.drop_delivery(&delivery, Some(&source_id)).await,
                Sent::Undeliverable => self.drop_delivery(&delivery, None).await,
            }
        }
    }

    /// Writes a rating into its file, unless the file already has it. A file that takes no
    /// rating leaves it Rufin's own.
    async fn write(&self, source_id: &SourceId, written: Option<i64>, delivery: &Delivery) -> Sent {
        let writes = configured_source(&self.shared.settings.load().sources, source_id)
            .is_ok_and(|configured| writes_ratings(&configured));
        let Some(source_key) = self.shared.catalog.source_key(source_id).filter(|_| writes) else {
            return Sent::Undeliverable;
        };
        if delivery.value.filter(|value| *value > 0) == written {
            return Sent::Accepted { keep_copy: false };
        }
        let Ok(source) = self.open_client(source_id.clone()).await else {
            return Sent::Later;
        };
        let rating = delivery.value.map(|value| (value / 10).clamp(0, 10) as u8);
        match source
            .write_file_track_rating(
                &self.shared.catalog,
                source_key,
                &delivery.media_uri,
                rating,
            )
            .await
        {
            RatingWrite::Written => Sent::Accepted { keep_copy: false },
            RatingWrite::Unwritable => Sent::Undeliverable,
            RatingWrite::Later => Sent::Later,
        }
    }

    /// Where a delivery goes: the file of a file source's track, or the server its URI names.
    async fn delivery_target(&self, delivery: &Delivery) -> Option<Target> {
        if delivery.kind == DeliveryKind::Rating
            && let Ok(Some((source_id, written))) = self
                .shared
                .catalog
                .file_track_rating(&delivery.media_uri)
                .await
        {
            return Some(Target::File(source_id, written));
        }
        let (source_id, kind, object_id) = library::source_entity_parts(&delivery.media_uri)?;
        let kind = match kind.as_str() {
            "track" => SourceEntityKind::Track,
            "album" => SourceEntityKind::Album,
            "artist" => SourceEntityKind::Artist,
            _ => return None,
        };
        Some(Target::Server(source_id, kind, object_id))
    }

    async fn send(
        &self,
        source_id: &SourceId,
        entity: SourceEntityKind,
        object_id: &str,
        delivery: &Delivery,
    ) -> Sent {
        let Some(kind) = self
            .configuration(source_id)
            .and_then(|configuration| configuration.source_kind())
            .filter(|kind| kind.delivers(delivery.kind))
        else {
            return Sent::Undeliverable;
        };
        let Ok(source) = self.open_client(source_id.clone()).await else {
            return Sent::Later;
        };
        let result = match delivery.kind {
            DeliveryKind::Favorite => {
                source
                    .set_favorite(entity, object_id, delivery.value == Some(1))
                    .await
            }
            DeliveryKind::Rating => {
                let rating = delivery.value.map(|value| (value / 10).clamp(0, 10) as u8);
                source.set_rating(object_id, rating).await
            }
        };
        match result {
            Ok(()) => Sent::Accepted {
                keep_copy: keeps_copy(kind, delivery),
            },
            // A login that expired is fixed by signing in again, not by dropping the value.
            Err(
                SourceError::Auth(_)
                | SourceError::Server {
                    status: 401 | 403, ..
                },
            ) => Sent::Later,
            Err(error) if source_error_is_temporary(&error) => Sent::Later,
            Err(_) => Sent::Refused,
        }
    }

    /// Drops a delivery. A favorite the server refused goes with Rufin's copy, so the source's
    /// own favorite shows again; a refused rating, and a value no source takes, stay Rufin's own.
    async fn drop_delivery(&self, delivery: &Delivery, refused_by: Option<&SourceId>) {
        info!(kind = ?delivery.kind, refused = refused_by.is_some(), "Delivery dropped");
        let refused_favorite = refused_by.filter(|_| delivery.kind == DeliveryKind::Favorite);
        match self
            .shared
            .catalog
            .drop_delivery(delivery, refused_favorite.is_none())
            .await
        {
            Ok(true) if refused_favorite.is_some() => {
                self.shared.send(SourceEvent::FavoriteRejected).await;
            }
            Ok(_) => {}
            Err(error) => self.shared.warn_nonfatal(&error.to_string()),
        }
    }
}

enum Sent {
    /// The source took it; `keep_copy` says whether Rufin keeps its own copy beside it.
    Accepted { keep_copy: bool },
    /// The source cannot take it now.
    Later,
    /// The server refused it.
    Refused,
    /// No configured source takes it.
    Undeliverable,
}

enum Target {
    /// The source, kind and object of a server item.
    Server(SourceId, SourceEntityKind, String),
    /// The source of a file source's track, and the rating its file has (1..100).
    File(SourceId, Option<i64>),
}
