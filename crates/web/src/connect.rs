use super::*;

pub(super) fn routes() -> Router<ProductHandles> {
    Router::new()
        .route("/api/connect", get(status).post(action))
        .route("/api/connect/events", get(events))
        .route("/api/connect/continuation", get(continuation))
        .route("/api/room", get(room).post(room_action))
        .route("/api/room/events", get(room_events))
        .route("/api/room/note", post(room_note))
}

#[derive(serde::Deserialize)]
struct RoomInvite {
    invite: String,
}

/// What a listener reads of an invite before joining.
async fn room_note(
    State(products): State<ProductHandles>,
    axum::Json(RoomInvite { invite }): axum::Json<RoomInvite>,
) -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "note": products.room.invite_note(invite) }))
}

async fn room(State(products): State<ProductHandles>) -> axum::Json<rufin_core::room::RoomStatus> {
    axum::Json(products.room.status())
}

async fn room_action(
    State(products): State<ProductHandles>,
    axum::Json(action): axum::Json<rufin_core::room::RoomAction>,
) -> Result<axum::Json<rufin_core::room::RoomStatus>, Error> {
    (products.room.execute(action).await)
        .map(axum::Json)
        .map_err(|failure| error(StatusCode::BAD_REQUEST, failure))
}

async fn room_events(State(products): State<ProductHandles>) -> Response<Body> {
    let stream =
        futures_util::stream::unfold(products.room.subscribe(), |mut updates| async move {
            updates.changed().await.ok()?;
            let data = serde_json::to_string(&*updates.borrow_and_update()).ok()?;
            Some((
                Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                    "event: room\ndata: {data}\n\n"
                )))),
                updates,
            ))
        });
    event_response(Body::new(StreamBody::new(stream)))
}

async fn continuation(
    State(products): State<ProductHandles>,
) -> axum::Json<Option<rufin_core::connect::Device>> {
    let status = products.connect.status();
    axum::Json(status.continuation.and_then(|continuation| {
        status
            .devices
            .into_iter()
            .find(|device| device.id == continuation.device)
    }))
}

async fn status(State(products): State<ProductHandles>) -> axum::Json<rufin_core::connect::Status> {
    axum::Json(products.connect.status())
}

async fn action(
    State(products): State<ProductHandles>,
    axum::Json(action): axum::Json<rufin_core::connect::Action>,
) -> Result<axum::Json<rufin_core::connect::Status>, Error> {
    products
        .connect
        .execute(action)
        .await
        .map(axum::Json)
        .map_err(|error| {
            let confirmation = match &error {
                rufin_core::connect::ActionError::ConfirmationRequired { confirmation, .. } => {
                    Some(confirmation)
                }
                _ => None,
            };
            (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({"error":error.to_string(), "confirmation":confirmation})),
            )
        })
}

async fn events(State(products): State<ProductHandles>) -> Response<Body> {
    let stream =
        futures_util::stream::unfold(products.connect.subscribe(), |mut updates| async move {
            updates.changed().await.ok()?;
            let data = serde_json::to_string(&*updates.borrow_and_update()).ok()?;
            Some((
                Ok::<_, Infallible>(Frame::data(Bytes::from(format!(
                    "event: connect\ndata: {data}\n\n"
                )))),
                updates,
            ))
        });
    event_response(Body::new(StreamBody::new(stream)))
}
