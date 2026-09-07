use axum::{
    extract::{Path, Query, State},
    response::sse::{Event, Sse},
    routing::{get, post},
    Json, Router,
};
use rand::RngExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::StreamExt;

use crate::error::AppError;
use crate::events::{ConferenceUnmuteEvent, KickedEvent, ModerationChangedEvent};
use crate::livekit::LiveKitClient;
use crate::presence;
use crate::routes::rate_limit;
use crate::routes::sql::row_to_json;
use crate::state::AppState;

use tracing::info;

// GET /:slug/info - safe room info (no auth)
async fn room_info(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
) -> Result<Json<Value>, AppError> {
    let conn = state.db.get()?;
    let room = tokio::task::spawn_blocking(move || {
        let mut stmt = conn.prepare(
            "SELECT id, name, slug, delivery_mode, waiting_room, noise_reduction, echo_cancellation, push_to_talk, \
             CASE WHEN password_hash IS NOT NULL AND password_hash != '' THEN 1 ELSE 0 END as has_password, \
             CASE WHEN stream_key_id IS NOT NULL THEN 1 ELSE 0 END as has_stream_key, \
             status, starts_at \
             FROM rooms WHERE slug = ?1",
        )?;
        let cols = &[
            "id",
            "name",
            "slug",
            "delivery_mode",
            "waiting_room",
            "noise_reduction",
            "echo_cancellation",
            "push_to_talk",
            "has_password",
            "has_stream_key",
            "status",
            "starts_at",
        ];
        let row = stmt
            .query_row(rusqlite::params![slug], |row| row_to_json(row, cols))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    AppError::NotFound("Room not found".into())
                }
                _ => AppError::Internal(e.to_string()),
            })?;
        Ok::<_, AppError>(row)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    Ok(Json(room))
}

#[derive(Deserialize)]
struct JoinBody {
    name: Option<String>,
    password: Option<String>,
    role: Option<String>,
    presenter_key: Option<String>,
}

// POST /:slug/join - join a room (public)
async fn join_room(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    Json(body): Json<JoinBody>,
) -> Result<Json<Value>, AppError> {
    let name = body
        .name
        .ok_or_else(|| AppError::BadRequest("Name is required".into()))?;

    let conn = state.db.get()?;
    let slug_clone = slug.clone();
    let room_data = tokio::task::spawn_blocking(move || {
        let mut stmt = conn.prepare(
            "SELECT r.id, r.name, r.slug, r.password_hash, r.presenter_key, \
             r.delivery_mode, r.waiting_room, r.status, \
             (r.expires_at IS NOT NULL AND r.expires_at <= CURRENT_TIMESTAMP) AS is_expired, \
             sk.key_token, r.noise_reduction, r.echo_cancellation, r.push_to_talk, \
             r.starts_at \
             FROM rooms r \
             LEFT JOIN stream_keys sk ON sk.id = r.stream_key_id \
             WHERE r.slug = ?1",
        )?;
        let result = stmt
            .query_row(rusqlite::params![slug_clone], |row| {
                Ok((
                    row.get::<_, String>(0)?,          // id
                    row.get::<_, String>(1)?,          // name
                    row.get::<_, String>(2)?,          // slug
                    row.get::<_, Option<String>>(3)?,  // password_hash
                    row.get::<_, Option<String>>(4)?,  // presenter_key
                    row.get::<_, String>(5)?,          // delivery_mode
                    row.get::<_, i32>(6)?,             // waiting_room
                    row.get::<_, String>(7)?,          // status
                    row.get::<_, i32>(8)?,             // is_expired
                    row.get::<_, Option<String>>(9)?,  // stream key_token
                    row.get::<_, i32>(10)?,            // noise_reduction
                    row.get::<_, i32>(11)?,            // echo_cancellation
                    row.get::<_, i32>(12)?,            // push_to_talk
                    row.get::<_, Option<String>>(13)?, // starts_at
                ))
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => AppError::NotFound("Room not found".into()),
                _ => AppError::Internal(e.to_string()),
            })?;
        Ok::<_, AppError>(result)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    let (
        room_id,
        room_name,
        _room_slug,
        password_hash,
        presenter_key,
        delivery_mode,
        waiting_room,
        status,
        is_expired,
        stream_key,
        noise_reduction,
        echo_cancellation,
        push_to_talk,
        starts_at,
    ) = room_data;

    // 410 if ended
    if status == "ended" {
        return Err(AppError::Gone("Room has ended".into()));
    }

    // 410 if expired. The comparison is done by SQLite against its own
    // CURRENT_TIMESTAMP (see the `is_expired` column above) rather than by a
    // hand-rolled UTC clock in Rust — one source of truth for "now", and the
    // same form `watch.rs` and the pollers already use.
    if is_expired == 1 {
        return Err(AppError::Gone("Room has expired".into()));
    }

    // A valid host link (correct presenter_key) bypasses the password gate
    // so the room password can be rotated for clients without invalidating
    // the host's bookmark.
    let is_valid_presenter = body.role.as_deref() == Some("presenter")
        && match (&presenter_key, &body.presenter_key) {
            (Some(stored), Some(provided)) => stored == provided,
            _ => false,
        };

    // 401 if password required but wrong (skipped for valid host links).
    if !is_valid_presenter {
        if let Some(ref hash) = password_hash {
            if !hash.is_empty() {
                let provided = body.password.clone().unwrap_or_default();
                if provided.is_empty() {
                    return Err(AppError::Unauthorized("Password required".into()));
                }
                let hash_clone = hash.clone();
                let valid =
                    tokio::task::spawn_blocking(move || bcrypt::verify(provided, &hash_clone))
                        .await
                        .map_err(|e| AppError::Internal(e.to_string()))?
                        .map_err(|_| AppError::Unauthorized("Wrong password".into()))?;
                if !valid {
                    return Err(AppError::Unauthorized("Wrong password".into()));
                }
            }
        }
    }

    // Check if name is kicked (case-insensitive)
    let conn = state.db.get()?;
    let room_id_clone = room_id.clone();
    let name_clone = name.clone();
    let is_kicked = tokio::task::spawn_blocking(move || {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM participants \
             WHERE room_id = ?1 AND LOWER(name) = LOWER(?2) AND is_kicked = 1",
            rusqlite::params![room_id_clone, name_clone],
            |row| row.get(0),
        )?;
        Ok::<_, rusqlite::Error>(count > 0)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    if is_kicked {
        return Err(AppError::Forbidden(
            "You have been kicked from this room".into(),
        ));
    }

    // Determine role
    let requested_role = body.role.as_deref().unwrap_or("viewer");
    let role = if requested_role == "presenter" {
        if let Some(ref pk) = presenter_key {
            if body.presenter_key.as_deref() == Some(pk.as_str()) {
                "presenter"
            } else {
                "viewer"
            }
        } else {
            "viewer"
        }
    } else {
        "viewer"
    };

    // Admission gating:
    //   - presenters (valid host link / admin entry) are always admitted, so a
    //     host can set up before the scheduled start.
    //   - a 'scheduled' room (issue #200) holds every other joiner until the
    //     start poller flips it to 'pending' and auto-admits them — regardless
    //     of the manual waiting-room setting.
    //   - otherwise the manual waiting room decides.
    let is_admitted: i32 = if role == "presenter" {
        1
    } else if status == "scheduled" {
        0
    } else if waiting_room == 0 {
        1
    } else {
        0
    };

    let participant_id = uuid::Uuid::new_v4().to_string();
    let token_bytes: [u8; 24] = rand::rng().random();
    let token: String = token_bytes.iter().map(|b| format!("{:02x}", b)).collect();

    let conn = state.db.get()?;
    let pid = participant_id.clone();
    let rid = room_id.clone();
    let tok = token.clone();
    let p_name = name.clone();
    let p_role = role.to_string();
    tokio::task::spawn_blocking(move || {
        conn.execute(
            "INSERT INTO participants (id, room_id, name, role, is_admitted, token) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![pid, rid, p_name, p_role, is_admitted, tok],
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    // New waiting joiner → tell connected presenters via WS so they can
    // admit without polling. (Auto-admit also fires so admin-page state
    // stays consistent.)
    let _ = state
        .events
        .moderation_changed
        .send(ModerationChangedEvent { slug: slug.clone() });

    Ok(Json(json!({
        "participant_id": participant_id,
        "token": token,
        "role": role,
        "admitted": is_admitted == 1,
        "delivery_mode": delivery_mode,
        "waiting_room": waiting_room != 0,
        "noise_reduction_default": noise_reduction != 0,
        "echo_cancellation_default": echo_cancellation != 0,
        "push_to_talk_default": push_to_talk != 0,
        "stream_key": stream_key,
        "room_name": room_name,
        "status": status,
        "starts_at": starts_at,
    })))
}

#[derive(Deserialize)]
struct StatusQuery {
    token: Option<String>,
}

/// Immediately admit a held viewer whose scheduled start has passed, when the
/// room's waiting room is off — so admission happens within the ~2s admission-poll
/// interval of `starts_at` instead of waiting for the 60s start poller
/// ([`crate::tasks::poll_starts`], which stays as the backstop and also does the
/// room `scheduled` -> `pending` flip). Idempotent; a no-op for waiting-room
/// rooms, kicked/ended rooms, or before the start. Returns whether it flipped
/// the flag, so the caller can nudge the host roster.
fn admit_if_started(conn: &rusqlite::Connection, participant_id: &str) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "UPDATE participants SET is_admitted = 1 \
         WHERE id = ?1 AND is_admitted = 0 AND is_kicked = 0 \
         AND EXISTS ( \
             SELECT 1 FROM rooms r \
             WHERE r.id = participants.room_id \
             AND r.waiting_room = 0 \
             AND r.status != 'ended' \
             AND r.starts_at IS NOT NULL \
             AND r.starts_at <= CURRENT_TIMESTAMP)",
        rusqlite::params![participant_id],
    )?;
    Ok(n > 0)
}

// GET /:slug/status/:participantId?token= - admission poll
async fn participant_status(
    State(state): State<Arc<AppState>>,
    Path((slug, participant_id)): Path<(String, String)>,
    Query(query): Query<StatusQuery>,
) -> Result<Json<Value>, AppError> {
    let token = query
        .token
        .ok_or_else(|| AppError::Unauthorized("Token required".into()))?;

    let conn = state.db.get()?;
    let slug_evt = slug.clone();
    let result = tokio::task::spawn_blocking(move || {
        let did_admit = admit_if_started(&conn, &participant_id)?;
        let mut stmt = conn.prepare(
            "SELECT p.is_admitted, p.is_kicked, r.status, \
             (r.starts_at IS NOT NULL AND r.starts_at <= CURRENT_TIMESTAMP) AS started \
             FROM participants p \
             JOIN rooms r ON r.id = p.room_id \
             WHERE p.id = ?1 AND p.token = ?2 AND r.slug = ?3",
        )?;
        let (is_admitted, is_kicked, room_status, started) = stmt
            .query_row(rusqlite::params![participant_id, token, slug], |row| {
                Ok((
                    row.get::<_, i32>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i32>(3)?,
                ))
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    AppError::NotFound("Participant not found".into())
                }
                _ => AppError::Internal(e.to_string()),
            })?;
        Ok::<_, AppError>((did_admit, is_admitted, is_kicked, room_status, started))
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    let (did_admit, is_admitted, is_kicked, room_status, started) = result;
    if did_admit {
        let _ = state
            .events
            .moderation_changed
            .send(ModerationChangedEvent { slug: slug_evt });
    }

    Ok(Json(json!({
        "admitted": is_admitted == 1,
        "kicked": is_kicked == 1,
        "room_status": room_status,
        "started": started == 1,
    })))
}

#[derive(Deserialize)]
struct WaitingEventsQuery {
    token: Option<String>,
}

/// Holds an SRT viewer's presence entry for the lifetime of their admission
/// SSE stream. When the stream is dropped (client disconnect / network loss)
/// this deregisters them and nudges the host roster to refresh — so a Farbplay
/// viewer disappears from the roster within ~the SSE poll interval of leaving.
struct SsePresenceGuard {
    slug: String,
    participant_id: String,
    moderation_changed: tokio::sync::broadcast::Sender<ModerationChangedEvent>,
}

impl Drop for SsePresenceGuard {
    fn drop(&mut self) {
        presence::remove(&self.slug, &self.participant_id);
        let _ = self.moderation_changed.send(ModerationChangedEvent {
            slug: self.slug.clone(),
        });
    }
}

// GET /:slug/waiting/events/:participantId?token= - SSE for waiting room
async fn waiting_events(
    State(state): State<Arc<AppState>>,
    Path((slug, participant_id)): Path<(String, String)>,
    Query(query): Query<WaitingEventsQuery>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, AppError> {
    let token = query
        .token
        .ok_or_else(|| AppError::Unauthorized("Token required".into()))?;

    // Validate token first
    let conn = state.db.get()?;
    let pid = participant_id.clone();
    let tok = token.clone();
    let s = slug.clone();
    tokio::task::spawn_blocking(move || {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM participants p \
             JOIN rooms r ON r.id = p.room_id \
             WHERE p.id = ?1 AND p.token = ?2 AND r.slug = ?3",
            rusqlite::params![pid, tok, s],
            |row| row.get(0),
        )?;
        if count == 0 {
            return Err(AppError::NotFound("Participant not found".into()));
        }
        Ok::<_, AppError>(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    // Register SRT presence for the lifetime of this SSE connection. Browser
    // waiting-room clients are registered too, but they stay is_admitted=0 until
    // they leave the waiting room, so they're filtered out of the host's
    // admitted (SRT-viewer) list. Nudge the roster now so an admitted viewer
    // who (re)opens the SSE appears immediately.
    presence::add(&slug, &participant_id);
    let _ = state
        .events
        .moderation_changed
        .send(ModerationChangedEvent { slug: slug.clone() });
    let guard = SsePresenceGuard {
        slug: slug.clone(),
        participant_id: participant_id.clone(),
        moderation_changed: state.events.moderation_changed.clone(),
    };

    let stream = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(
        std::time::Duration::from_secs(2),
    ))
    .map(move |_| {
        let state = state.clone();
        let participant_id = participant_id.clone();
        let slug = slug.clone();
        (state, participant_id, slug)
    })
    .then(|(state, participant_id, slug)| async move {
        let conn = match state.db.get() {
            Ok(c) => c,
            Err(_) => {
                return Ok(Event::default().event("ping").data("{}"));
            }
        };
        let slug_evt = slug.clone();
        let result = tokio::task::spawn_blocking(move || {
            let did_admit = admit_if_started(&conn, &participant_id)?;
            let mut stmt = conn.prepare(
                "SELECT p.is_admitted, p.is_kicked, r.status, \
                 (r.starts_at IS NOT NULL AND r.starts_at <= CURRENT_TIMESTAMP) AS started \
                 FROM participants p \
                 JOIN rooms r ON r.id = p.room_id \
                 WHERE p.id = ?1 AND r.slug = ?2",
            )?;
            let (is_admitted, is_kicked, room_status, started) =
                stmt.query_row(rusqlite::params![participant_id, slug], |row| {
                    Ok((
                        row.get::<_, i32>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i32>(3)?,
                    ))
                })?;
            Ok::<_, rusqlite::Error>((did_admit, is_admitted, is_kicked, room_status, started))
        })
        .await;

        match result {
            Ok(Ok((did_admit, is_admitted, is_kicked, room_status, started))) => {
                if did_admit {
                    let _ = state
                        .events
                        .moderation_changed
                        .send(ModerationChangedEvent { slug: slug_evt });
                }
                if is_kicked == 1 {
                    Ok(Event::default().event("kicked").data(json!({}).to_string()))
                } else if room_status == "ended" {
                    Ok(Event::default()
                        .event("room_ended")
                        .data(json!({}).to_string()))
                } else if is_admitted == 1 {
                    Ok(Event::default()
                        .event("admitted")
                        .data(json!({}).to_string()))
                } else {
                    // Still waiting. Carry room status + whether the scheduled
                    // start has passed, so a held waiting-room client can switch
                    // from the "starting soon" screen to the waiting-room screen
                    // once the room is running (issue #200 follow-up).
                    Ok(Event::default().event("ping").data(
                        json!({ "room_status": room_status, "started": started == 1 }).to_string(),
                    ))
                }
            }
            // Participant not found (deleted) - send ping; client will handle
            _ => Ok(Event::default().event("ping").data(json!({}).to_string())),
        }
    });

    // Carry the presence guard with the stream so it deregisters the viewer when
    // the SSE connection is dropped (the stream is dropped on client disconnect).
    let stream = stream.map(move |ev| {
        let _keep = &guard;
        ev
    });

    Ok(Sse::new(stream))
}

#[derive(Deserialize)]
struct LivekitTokenQuery {
    #[serde(rename = "participantId")]
    participant_id: Option<String>,
    token: Option<String>,
}

// GET /:slug/livekit-token?participantId=&token= - get LiveKit access token
async fn livekit_token(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    Query(query): Query<LivekitTokenQuery>,
) -> Result<Json<Value>, AppError> {
    let participant_id = query
        .participant_id
        .ok_or_else(|| AppError::BadRequest("participantId required".into()))?;
    let token = query
        .token
        .ok_or_else(|| AppError::Unauthorized("Token required".into()))?;

    let conn = state.db.get()?;
    let slug_clone = slug.clone();
    let pid = participant_id.clone();
    let participant_data = tokio::task::spawn_blocking(move || {
        // Ended and expired rooms collapse to "no row" -> 404, matching
        // `watch.rs`. A LiveKit token is a live A/V publish grant, so a session
        // that is over must not keep minting them; the room's own `expires_at`
        // capped the token's lifetime but nothing stopped it being issued.
        // `expires_at` is a UTC "YYYY-MM-DD HH:MM:SS" string (see
        // `rooms::normalize_datetime`), so it compares against CURRENT_TIMESTAMP.
        let mut stmt = conn.prepare(
            "SELECT p.name, p.role, p.is_admitted, p.is_kicked, r.slug, \
             CAST(strftime('%s', r.expires_at) AS INTEGER) \
             FROM participants p \
             JOIN rooms r ON r.id = p.room_id \
             WHERE p.id = ?1 AND p.token = ?2 AND r.slug = ?3 \
               AND r.status != 'ended' \
               AND (r.expires_at IS NULL OR r.expires_at > CURRENT_TIMESTAMP)",
        )?;
        let result = stmt
            .query_row(rusqlite::params![pid, token, slug_clone], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i32>(2)?,
                    row.get::<_, i32>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                ))
            })
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    AppError::NotFound("Participant not found".into())
                }
                _ => AppError::Internal(e.to_string()),
            })?;
        Ok::<_, AppError>(result)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    let (name, role, is_admitted, is_kicked, room_slug, expires_at_unix) = participant_data;

    if is_kicked == 1 {
        return Err(AppError::Forbidden("You have been kicked".into()));
    }
    if is_admitted == 0 {
        return Err(AppError::Forbidden("Not yet admitted".into()));
    }

    // SQLite parsed `expires_at` for us; negatives can't be a valid room
    // expiry, so they collapse to "no expiry" and the token's own 1h cap wins.
    let expires_at_unix = expires_at_unix.and_then(|v| u64::try_from(v).ok());

    let livekit = LiveKitClient::new(&state.config, state.http_client.clone());
    let lk_token = livekit
        .create_access_token(&participant_id, &name, &room_slug, &role, expires_at_unix)
        .map_err(AppError::Internal)?;

    Ok(Json(
        json!({ "token": lk_token, "url": state.config.livekit_url }),
    ))
}

#[derive(Deserialize)]
struct KickBody {
    #[serde(rename = "participantId")]
    participant_id: Option<String>,
    token: Option<String>,
    #[serde(rename = "targetId")]
    target_id: Option<String>,
}

// POST /:slug/conference/kick - kick a participant (presenter action)
async fn kick_participant(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    Json(body): Json<KickBody>,
) -> Result<Json<Value>, AppError> {
    let participant_id = body
        .participant_id
        .ok_or_else(|| AppError::BadRequest("participantId required".into()))?;
    let token = body
        .token
        .ok_or_else(|| AppError::Unauthorized("Token required".into()))?;
    let target_id = body
        .target_id
        .ok_or_else(|| AppError::BadRequest("targetId required".into()))?;

    // Shares the presenter gate with the rest of the moderation endpoints, so
    // the `is_kicked` half of it can't drift out of this one.
    require_presenter(
        &state,
        &slug,
        Some(participant_id.clone()),
        Some(token.clone()),
    )
    .await?;

    // Mark target as kicked
    let conn = state.db.get()?;
    let tid = target_id.clone();
    let slug_clone = slug.clone();
    tokio::task::spawn_blocking(move || {
        let changes = conn.execute(
            "UPDATE participants SET is_kicked = 1 \
             WHERE id = ?1 AND room_id = (SELECT id FROM rooms WHERE slug = ?2)",
            rusqlite::params![tid, slug_clone],
        )?;
        if changes == 0 {
            return Err(AppError::NotFound("Target participant not found".into()));
        }
        Ok::<_, AppError>(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    info!(
        room_slug = %slug,
        actor_id = %participant_id,
        target_id = %target_id,
        action = "kick",
        "participant kicked",
    );

    let _ = state.events.participant_kicked.send(KickedEvent {
        slug: slug.clone(),
        participant_id: target_id.clone(),
    });
    let _ = state
        .events
        .moderation_changed
        .send(ModerationChangedEvent { slug: slug.clone() });

    // Remove from LiveKit so the victim's A/V actually stops. DB flag + WS
    // force-close already happened; this is the call that matters for the
    // audio/video channel. Retry once on transient failure, then log loudly.
    let livekit = LiveKitClient::new(&state.config, state.http_client.clone());
    if let Err(first) = livekit.remove_participant(&slug, &target_id).await {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if let Err(second) = livekit.remove_participant(&slug, &target_id).await {
            tracing::error!(
                room_slug = %slug,
                target_id = %target_id,
                first_error = %first,
                error = %second,
                "LiveKit remove_participant failed after retry; victim may still be publishing A/V",
            );
        }
    }

    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct MuteBody {
    #[serde(rename = "participantId")]
    participant_id: Option<String>,
    token: Option<String>,
    #[serde(rename = "targetId")]
    target_id: Option<String>,
    #[serde(rename = "trackSid")]
    track_sid: Option<String>,
    muted: Option<bool>,
}

// POST /:slug/conference/mute - mute a participant's track (presenter action)
async fn mute_participant(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    Json(body): Json<MuteBody>,
) -> Result<Json<Value>, AppError> {
    let participant_id = body
        .participant_id
        .ok_or_else(|| AppError::BadRequest("participantId required".into()))?;
    let token = body
        .token
        .ok_or_else(|| AppError::Unauthorized("Token required".into()))?;
    let target_id = body
        .target_id
        .ok_or_else(|| AppError::BadRequest("targetId required".into()))?;
    let track_sid = body
        .track_sid
        .ok_or_else(|| AppError::BadRequest("trackSid required".into()))?;
    let muted = body.muted.unwrap_or(true);

    require_presenter(
        &state,
        &slug,
        Some(participant_id.clone()),
        Some(token.clone()),
    )
    .await?;

    if muted {
        // Server-side mute is enforced by LiveKit and the target's SDK
        // auto-mutes on receipt.
        let livekit = LiveKitClient::new(&state.config, state.http_client.clone());
        livekit
            .mute_published_track(&slug, &target_id, &track_sid, true)
            .await
            .map_err(AppError::Internal)?;
    } else {
        // LiveKit cannot force-unmute a track (privacy), so calling it here is
        // a misleading no-op. Push an unmute request to the target instead —
        // their browser prompts and re-enables the mic itself on consent.
        let _ = state.events.conference_unmute.send(ConferenceUnmuteEvent {
            slug: slug.clone(),
            participant_id: target_id.clone(),
        });
    }

    info!(
        room_slug = %slug,
        actor_id = %participant_id,
        target_id = %target_id,
        track_sid = %track_sid,
        muted,
        action = "mute",
        "participant track muted",
    );

    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Presenter-gated moderation endpoints — mirror the admin admit/kick/unkick
// surface so a host link holder can run the session without admin access.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PresenterAuthQuery {
    #[serde(rename = "participantId")]
    participant_id: Option<String>,
    token: Option<String>,
}

#[derive(Deserialize)]
struct PresenterAuthBody {
    #[serde(rename = "participantId")]
    participant_id: Option<String>,
    token: Option<String>,
}

/// Verify (participantId, token, slug) belongs to an active presenter and
/// return the underlying room_id for use in subsequent queries. Used by every
/// presenter-gated endpoint in this file.
///
/// `is_kicked` is part of the check: a kick sets the flag but leaves `role`
/// alone, so reading role by itself left a kicked presenter's token still able
/// to admit, kick and unkick.
async fn require_presenter(
    state: &Arc<AppState>,
    slug: &str,
    participant_id: Option<String>,
    token: Option<String>,
) -> Result<String, AppError> {
    let participant_id =
        participant_id.ok_or_else(|| AppError::BadRequest("participantId required".into()))?;
    let token = token.ok_or_else(|| AppError::Unauthorized("Token required".into()))?;
    let slug = slug.to_string();

    let conn = state.db.get()?;
    let (role, is_kicked, room_id): (String, i32, String) =
        tokio::task::spawn_blocking(move || {
            conn.query_row(
                "SELECT p.role, p.is_kicked, p.room_id FROM participants p \
                 JOIN rooms r ON r.id = p.room_id \
                 WHERE p.id = ?1 AND p.token = ?2 AND r.slug = ?3",
                rusqlite::params![participant_id, token, slug],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => {
                    AppError::NotFound("Participant not found".into())
                }
                _ => AppError::Internal(e.to_string()),
            })
        })
        .await
        .map_err(|e| AppError::Internal(e.to_string()))??;

    if is_kicked == 1 {
        return Err(AppError::Forbidden(
            "You have been kicked from this room".into(),
        ));
    }
    if role != "presenter" {
        return Err(AppError::Forbidden("Presenter role required".into()));
    }
    Ok(room_id)
}

async fn list_participants_by_state(
    state: &Arc<AppState>,
    room_id: String,
    waiting: bool,
) -> Result<Vec<Value>, AppError> {
    let sql = if waiting {
        "SELECT id, name, role, joined_at FROM participants \
         WHERE room_id = ?1 AND is_admitted = 0 AND is_kicked = 0 \
         ORDER BY joined_at ASC"
    } else {
        "SELECT id, name, role, joined_at FROM participants \
         WHERE room_id = ?1 AND is_kicked = 1 \
         ORDER BY joined_at ASC"
    };
    let conn = state.db.get()?;
    let rows = tokio::task::spawn_blocking(move || {
        let mut stmt = conn.prepare(sql)?;
        let cols = &["id", "name", "role", "joined_at"];
        let rows = stmt
            .query_map(rusqlite::params![room_id], |row| row_to_json(row, cols))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok::<_, rusqlite::Error>(rows)
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;
    Ok(rows)
}

// GET /:slug/conference/waiting
async fn conf_get_waiting(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    Query(q): Query<PresenterAuthQuery>,
) -> Result<Json<Vec<Value>>, AppError> {
    let room_id = require_presenter(&state, &slug, q.participant_id, q.token).await?;
    let rows = list_participants_by_state(&state, room_id, true).await?;
    Ok(Json(rows))
}

// GET /:slug/conference/kicked
async fn conf_get_kicked(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    Query(q): Query<PresenterAuthQuery>,
) -> Result<Json<Vec<Value>>, AppError> {
    let room_id = require_presenter(&state, &slug, q.participant_id, q.token).await?;
    let rows = list_participants_by_state(&state, room_id, false).await?;
    Ok(Json(rows))
}

// POST /:slug/conference/admit/:targetId
async fn conf_admit(
    State(state): State<Arc<AppState>>,
    Path((slug, target_id)): Path<(String, String)>,
    Json(body): Json<PresenterAuthBody>,
) -> Result<Json<Value>, AppError> {
    let room_id = require_presenter(&state, &slug, body.participant_id, body.token).await?;
    let conn = state.db.get()?;
    let rid = room_id.clone();
    let tid = target_id.clone();
    tokio::task::spawn_blocking(move || {
        let changes = conn.execute(
            "UPDATE participants SET is_admitted = 1 WHERE id = ?1 AND room_id = ?2",
            rusqlite::params![tid, rid],
        )?;
        if changes == 0 {
            return Err(AppError::NotFound("Participant not found".into()));
        }
        Ok::<_, AppError>(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    let _ = state
        .events
        .moderation_changed
        .send(ModerationChangedEvent { slug: slug.clone() });

    Ok(Json(json!({ "ok": true })))
}

// POST /:slug/conference/admit-all
async fn conf_admit_all(
    State(state): State<Arc<AppState>>,
    Path(slug): Path<String>,
    Json(body): Json<PresenterAuthBody>,
) -> Result<Json<Value>, AppError> {
    let room_id = require_presenter(&state, &slug, body.participant_id, body.token).await?;
    let conn = state.db.get()?;
    let rid = room_id.clone();
    let count: usize = tokio::task::spawn_blocking(move || {
        conn.execute(
            "UPDATE participants SET is_admitted = 1 \
             WHERE room_id = ?1 AND is_admitted = 0 AND is_kicked = 0",
            rusqlite::params![rid],
        )
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
    .map_err(|e| AppError::Internal(e.to_string()))?;

    let _ = state
        .events
        .moderation_changed
        .send(ModerationChangedEvent { slug: slug.clone() });

    Ok(Json(json!({ "ok": true, "count": count })))
}

// POST /:slug/conference/unkick/:targetId
async fn conf_unkick(
    State(state): State<Arc<AppState>>,
    Path((slug, target_id)): Path<(String, String)>,
    Json(body): Json<PresenterAuthBody>,
) -> Result<Json<Value>, AppError> {
    let room_id = require_presenter(&state, &slug, body.participant_id, body.token).await?;
    let conn = state.db.get()?;
    let rid = room_id.clone();
    let tid = target_id.clone();
    tokio::task::spawn_blocking(move || {
        let changes = conn.execute(
            "UPDATE participants SET is_kicked = 0 WHERE id = ?1 AND room_id = ?2",
            rusqlite::params![tid, rid],
        )?;
        if changes == 0 {
            return Err(AppError::NotFound("Participant not found".into()));
        }
        Ok::<_, AppError>(())
    })
    .await
    .map_err(|e| AppError::Internal(e.to_string()))??;

    let _ = state
        .events
        .moderation_changed
        .send(ModerationChangedEvent { slug: slug.clone() });

    Ok(Json(json!({ "ok": true })))
}

pub fn router() -> Router<Arc<AppState>> {
    let join_handler = if rate_limit::enabled() {
        post(join_room).layer(rate_limit::join_layer())
    } else {
        post(join_room)
    };
    Router::new()
        .route("/{slug}/info", get(room_info))
        .route("/{slug}/join", join_handler)
        .route("/{slug}/status/{participantId}", get(participant_status))
        .route(
            "/{slug}/waiting/events/{participantId}",
            get(waiting_events),
        )
        .route("/{slug}/livekit-token", get(livekit_token))
        .route("/{slug}/conference/kick", post(kick_participant))
        .route("/{slug}/conference/mute", post(mute_participant))
        .route("/{slug}/conference/waiting", get(conf_get_waiting))
        .route("/{slug}/conference/kicked", get(conf_get_kicked))
        .route("/{slug}/conference/admit/{targetId}", post(conf_admit))
        .route("/{slug}/conference/admit-all", post(conf_admit_all))
        .route("/{slug}/conference/unkick/{targetId}", post(conf_unkick))
}
