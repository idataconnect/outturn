//! A guesthouse that never was, for testing the things that talk to one.
//!
//! Every document here uses the same worked example -- Hollowbrook House, a
//! small place with a few rooms -- and this is that example made real enough to
//! call. It exists because the interesting parts of this platform are the seams
//! between tiers, and a seam tested in-process is a seam not tested: a webhook
//! delivered by a test's own request never crosses a socket, and an agent's
//! outbound call that is stubbed never reaches an egress rule.
//!
//! So it does four things, and each one is a seam somebody else needs:
//!
//! **It sends webhooks.** A booking made here POSTs into outturn, signed the
//! way a real sender signs, so the inbound path is exercised by something
//! outside the process rather than by a `Request::builder()`.
//!
//! **It answers a REST API.** An agent reaching back to check availability or
//! confirm a reservation goes through `fetch_url`, the gateway, the workspace's
//! egress rules and a real connection -- none of which a stub visits.
//!
//! **It says what it holds.** A test can assert the booking exists, rather than
//! asserting only that a turn finished, which is the difference between
//! checking work happened and checking it was attempted.
//!
//! **It serves its own OpenAPI document**, so the wizard that turns a
//! specification into a skill has a real one to read, and the skill that
//! results has a real service to be judged against.
//!
//! Everything it serves carries `x-outturn-fixture`, for the same reason the
//! mock provider marks its own responses: somebody finding this data later
//! should be able to tell it is not real without knowing this file exists.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A room, as the guesthouse thinks of one.
#[derive(Debug, Clone, Serialize)]
struct Room {
    id: String,
    name: String,
    sleeps: u32,
    /// Pence, so nothing here does floating-point money.
    rate_pence: u32,
}

#[derive(Debug, Clone, Serialize)]
struct Booking {
    id: String,
    room_id: String,
    guest_name: String,
    arrival: NaiveDate,
    departure: NaiveDate,
    /// What the whole stay costs, so a caller is not asked to multiply.
    total_pence: u32,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct NewBooking {
    room_id: String,
    guest_name: String,
    arrival: NaiveDate,
    departure: NaiveDate,
}

#[derive(Debug, Deserialize)]
struct AvailabilityQuery {
    arrival: NaiveDate,
    departure: NaiveDate,
}

struct Fixture {
    rooms: Vec<Room>,
    bookings: Mutex<Vec<Booking>>,
    /// Where a booking is announced, and what it is signed with. Absent means
    /// the guesthouse keeps its news to itself, which is the default: a
    /// fixture that POSTs somewhere on startup is a fixture that fails in
    /// somebody's cluster for reasons they did not ask about.
    webhook: Option<WebhookTarget>,
    client: reqwest::Client,
}

#[derive(Clone)]
struct WebhookTarget {
    url: String,
    /// The scheme this sender uses, matching what a trigger declares.
    /// `hmac` signs the body; `shared_secret` sends the credential itself.
    scheme: String,
    secret: String,
}

const FIXTURE_HEADER: &str = "x-outturn-fixture";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let webhook = match std::env::var("HOLLOWBROOK_WEBHOOK_URL") {
        Ok(url) if !url.is_empty() => {
            let scheme =
                std::env::var("HOLLOWBROOK_WEBHOOK_SCHEME").unwrap_or_else(|_| "hmac".to_string());
            let secret = std::env::var("HOLLOWBROOK_WEBHOOK_SECRET").unwrap_or_default();
            if secret.is_empty() {
                tracing::warn!("HOLLOWBROOK_WEBHOOK_URL set with no secret; not announcing");
                None
            } else {
                tracing::info!(url = %url, scheme = %scheme, "bookings will be announced");
                Some(WebhookTarget {
                    url,
                    scheme,
                    secret,
                })
            }
        }
        _ => {
            tracing::info!("no HOLLOWBROOK_WEBHOOK_URL; bookings will not be announced");
            None
        }
    };

    let state = Arc::new(Fixture {
        rooms: vec![
            Room {
                id: "garden".into(),
                name: "Garden Room".into(),
                sleeps: 2,
                rate_pence: 14_500,
            },
            Room {
                id: "orchard".into(),
                name: "Orchard Room".into(),
                sleeps: 2,
                rate_pence: 13_000,
            },
            Room {
                id: "loft".into(),
                name: "The Loft".into(),
                sleeps: 4,
                rate_pence: 19_500,
            },
        ],
        bookings: Mutex::new(Vec::new()),
        webhook,
        client: reqwest::Client::new(),
    });

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(|| async { "ok" }))
        .route("/openapi.json", get(openapi))
        .route("/rooms", get(list_rooms))
        .route("/availability", get(availability))
        .route("/bookings", get(list_bookings).post(create_booking))
        .route("/bookings/{id}", get(get_booking))
        // Resets between tests, so one test's bookings are not another's
        // availability. Not something a real service would offer, which is
        // why it is named for what it is.
        .route("/fixture/reset", post(reset))
        .with_state(state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8084);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .expect("bind");
    tracing::info!(port, "hollowbrook listening");
    axum::serve(listener, app).await.expect("serve");
}

fn marked<T: Serialize>(body: T) -> impl IntoResponse {
    ([(FIXTURE_HEADER, "hollowbrook")], Json(body))
}

async fn list_rooms(State(state): State<Arc<Fixture>>) -> impl IntoResponse {
    marked(serde_json::json!({ "rooms": state.rooms }))
}

/// Which rooms are free for a stay.
///
/// A room is taken when an existing booking overlaps the dates asked about,
/// where touching at the edges is not overlapping -- somebody leaving on the
/// 5th frees the room for somebody arriving on the 5th, which is how a
/// guesthouse works and is the kind of boundary an agent gets wrong.
async fn availability(
    State(state): State<Arc<Fixture>>,
    Query(q): Query<AvailabilityQuery>,
) -> impl IntoResponse {
    if q.departure <= q.arrival {
        return (
            StatusCode::BAD_REQUEST,
            [(FIXTURE_HEADER, "hollowbrook")],
            Json(serde_json::json!({
                "error": "departure must be after arrival",
            })),
        )
            .into_response();
    }

    let bookings = state.bookings.lock().expect("bookings");
    let nights = (q.departure - q.arrival).num_days().max(0) as u32;
    let free: Vec<_> = state
        .rooms
        .iter()
        .filter(|room| {
            !bookings
                .iter()
                .any(|b| b.room_id == room.id && b.arrival < q.departure && q.arrival < b.departure)
        })
        .map(|room| {
            serde_json::json!({
                "room_id": room.id,
                "name": room.name,
                "sleeps": room.sleeps,
                "rate_pence": room.rate_pence,
                "total_pence": room.rate_pence * nights,
            })
        })
        .collect();

    (
        StatusCode::OK,
        [(FIXTURE_HEADER, "hollowbrook")],
        Json(serde_json::json!({
            "arrival": q.arrival,
            "departure": q.departure,
            "nights": nights,
            "available": free,
        })),
    )
        .into_response()
}

async fn list_bookings(State(state): State<Arc<Fixture>>) -> impl IntoResponse {
    let bookings = state.bookings.lock().expect("bookings").clone();
    marked(serde_json::json!({ "bookings": bookings }))
}

async fn get_booking(
    State(state): State<Arc<Fixture>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let found = state
        .bookings
        .lock()
        .expect("bookings")
        .iter()
        .find(|b| b.id == id)
        .cloned();
    match found {
        Some(b) => (StatusCode::OK, [(FIXTURE_HEADER, "hollowbrook")], Json(b)).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [(FIXTURE_HEADER, "hollowbrook")],
            Json(serde_json::json!({ "error": "no such booking" })),
        )
            .into_response(),
    }
}

/// Takes a booking, and tells outturn about it.
///
/// The announcement is what makes this a webhook sender rather than only an
/// API: a test that wants the inbound path exercised makes a booking here and
/// the delivery arrives from outside the process, over a socket, signed.
async fn create_booking(
    State(state): State<Arc<Fixture>>,
    Json(input): Json<NewBooking>,
) -> impl IntoResponse {
    let Some(room) = state.rooms.iter().find(|r| r.id == input.room_id) else {
        return (
            StatusCode::NOT_FOUND,
            [(FIXTURE_HEADER, "hollowbrook")],
            Json(serde_json::json!({ "error": "no such room" })),
        )
            .into_response();
    };
    if input.departure <= input.arrival {
        return (
            StatusCode::BAD_REQUEST,
            [(FIXTURE_HEADER, "hollowbrook")],
            Json(serde_json::json!({ "error": "departure must be after arrival" })),
        )
            .into_response();
    }

    let booking = {
        let mut bookings = state.bookings.lock().expect("bookings");
        let clash = bookings.iter().any(|b| {
            b.room_id == input.room_id && b.arrival < input.departure && input.arrival < b.departure
        });
        if clash {
            return (
                StatusCode::CONFLICT,
                [(FIXTURE_HEADER, "hollowbrook")],
                Json(serde_json::json!({ "error": "that room is taken for those dates" })),
            )
                .into_response();
        }
        let nights = (input.departure - input.arrival).num_days().max(0) as u32;
        let booking = Booking {
            id: format!("bk_{}", &Uuid::now_v7().simple().to_string()[..12]),
            room_id: input.room_id,
            guest_name: input.guest_name,
            arrival: input.arrival,
            departure: input.departure,
            total_pence: room.rate_pence * nights,
            created_at: Utc::now(),
        };
        bookings.push(booking.clone());
        booking
    };

    announce(&state, &booking).await;

    (
        StatusCode::CREATED,
        [(FIXTURE_HEADER, "hollowbrook")],
        Json(booking),
    )
        .into_response()
}

/// POSTs a booking to whoever is listening, signed the way they asked.
///
/// Failures are logged and not retried. A fixture that retried would hide the
/// thing a test is usually checking -- that the first delivery was accepted --
/// behind a second attempt that happened to work.
async fn announce(state: &Fixture, booking: &Booking) {
    let Some(target) = &state.webhook else {
        return;
    };

    let body = serde_json::json!({
        "event": "booking.created",
        "booking": booking,
    })
    .to_string();

    let mut req = state
        .client
        .post(&target.url)
        .header("content-type", "application/json")
        .header(FIXTURE_HEADER, "hollowbrook");

    match target.scheme.as_str() {
        "shared_secret" => {
            req = req.header("x-outturn-token", &target.secret);
        }
        _ => {
            // Signed over the bytes being sent, not over a reserialisation of
            // them: the receiver verifies what arrived, and two JSON documents
            // that mean the same thing have different bytes.
            let timestamp = Utc::now().timestamp().to_string();
            let signed = format!("{timestamp}.{body}");
            let digest = hmac_sha256(target.secret.as_bytes(), signed.as_bytes());
            req = req
                .header("x-outturn-timestamp", &timestamp)
                .header("x-outturn-signature", format!("sha256={digest}"));
        }
    }

    match req.body(body).send().await {
        Ok(res) => tracing::info!(
            booking = %booking.id,
            status = res.status().as_u16(),
            "announced a booking"
        ),
        Err(e) => tracing::warn!(booking = %booking.id, error = %e, "could not announce"),
    }
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256>>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(message);
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn reset(State(state): State<Arc<Fixture>>) -> impl IntoResponse {
    state.bookings.lock().expect("bookings").clear();
    marked(serde_json::json!({ "reset": true }))
}

/// The service's own specification.
///
/// Served rather than committed as a file so it cannot drift from the routes
/// above without somebody noticing -- and because the wizard's job is to read
/// one from a URL, which is what most services offer.
async fn openapi(State(_state): State<Arc<Fixture>>) -> impl IntoResponse {
    let doc = serde_json::json!({
      "openapi": "3.0.3",
      "info": {
        "title": "Hollowbrook House",
        "version": "1.0.0",
        "description": "Rooms and bookings for a small guesthouse. A fixture: nothing here is real."
      },
      "paths": {
        "/rooms": {
          "get": {
            "operationId": "listRooms",
            "summary": "List every room, with what it sleeps and what it costs a night.",
            "tags": ["rooms"],
            "responses": { "200": { "description": "The rooms." } }
          }
        },
        "/availability": {
          "get": {
            "operationId": "checkAvailability",
            "summary": "Which rooms are free for a stay, and what the stay would cost.",
            "tags": ["rooms"],
            "parameters": [
              {
                "name": "arrival", "in": "query", "required": true,
                "schema": { "type": "string", "format": "date" },
                "description": "First night, as YYYY-MM-DD."
              },
              {
                "name": "departure", "in": "query", "required": true,
                "schema": { "type": "string", "format": "date" },
                "description": "Morning of departure, as YYYY-MM-DD. Must be after arrival."
              }
            ],
            "responses": {
              "200": { "description": "Rooms free for those dates." },
              "400": { "description": "Departure is not after arrival." }
            }
          }
        },
        "/bookings": {
          "get": {
            "operationId": "listBookings",
            "summary": "Every booking currently held.",
            "tags": ["bookings"],
            "responses": { "200": { "description": "The bookings." } }
          },
          "post": {
            "operationId": "createBooking",
            "summary": "Reserve a room for a guest.",
            "tags": ["bookings"],
            "requestBody": {
              "required": true,
              "content": {
                "application/json": {
                  "schema": {
                    "type": "object",
                    "required": ["room_id", "guest_name", "arrival", "departure"],
                    "properties": {
                      "room_id": { "type": "string", "description": "From /rooms, e.g. garden." },
                      "guest_name": { "type": "string" },
                      "arrival": { "type": "string", "format": "date" },
                      "departure": { "type": "string", "format": "date" }
                    }
                  }
                }
              }
            },
            "responses": {
              "201": { "description": "The booking that was made." },
              "404": { "description": "No such room." },
              "409": { "description": "That room is taken for those dates." }
            }
          }
        },
        "/bookings/{id}": {
          "get": {
            "operationId": "getBooking",
            "summary": "One booking, by the id returned when it was made.",
            "tags": ["bookings"],
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }
            ],
            "responses": {
              "200": { "description": "The booking." },
              "404": { "description": "No such booking." }
            }
          }
        }
      }
    });
    marked(doc)
}
