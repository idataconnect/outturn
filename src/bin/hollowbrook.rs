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

use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
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
    /// What the room is actually like, which `/rooms` deliberately does not
    /// return.
    ///
    /// Two levels of detail, which is what most REST APIs do and what an agent
    /// has to learn to navigate: the list carries enough to choose between
    /// rooms, and `/rooms/{id}` carries everything about one. Returning this
    /// from the list would make three descriptions arrive whenever somebody
    /// asked what rooms there are, which is the cost the split exists to
    /// avoid.
    #[serde(skip_serializing)]
    description: String,
}

/// A room with everything, for `/rooms/{id}`.
///
/// A separate shape rather than a flag on `Room`, so the field that the list
/// must not return cannot be returned by it accidentally.
#[derive(Debug, Clone, Serialize)]
struct RoomDetail {
    id: String,
    name: String,
    sleeps: u32,
    rate_pence: u32,
    description: String,
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

/// A way to take money from a guest, as the guesthouse records one.
///
/// Deliberately opaque. A payment account number stands for a card the
/// guesthouse already holds, and nothing here is a card number: an agent that
/// could read one would be an agent whose transcript contains one, and a
/// transcript is replayed to a model on every later turn. So the agent handles a
/// handle, the guesthouse knows what it points at, and the interesting part --
/// that a charge needs a person's say-so -- does not require the risky part.
#[derive(Debug, Clone, Serialize)]
struct PaymentAccount {
    /// `pa_` and four digits. Quoted to a guest and safe in a transcript.
    id: String,
    /// Whose it is, for the guesthouse's own records.
    holder: String,
    /// What the guest would recognise, e.g. "Visa ending 4471". Not a number.
    label: String,
}

#[derive(Debug, Clone, Serialize)]
struct Charge {
    id: String,
    payment_account_id: String,
    booking_id: String,
    amount_pence: u32,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct NewCharge {
    payment_account_id: String,
    booking_id: String,
    amount_pence: u32,
    /// What makes a repeat safe to refuse.
    ///
    /// The one operation here that takes money is the one where a retry has to
    /// be distinguishable from a second charge -- see docs/idempotency.md, which
    /// calls a recipient that accepts a key and stores it the best outcome
    /// available. This fixture is that recipient: a second charge quoting a key
    /// it has seen returns the first charge rather than taking the money twice.
    idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AvailabilityQuery {
    arrival: NaiveDate,
    departure: NaiveDate,
}

struct Fixture {
    rooms: Vec<Room>,
    bookings: Mutex<Vec<Booking>>,
    /// The cards on file, by handle. Fixed, because a guesthouse's payment
    /// accounts are not something an agent creates.
    payment_accounts: Vec<PaymentAccount>,
    /// What has been charged, so a test can assert money moved rather than
    /// asserting a turn finished.
    charges: Mutex<Vec<Charge>>,
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
                description: "On the ground floor at the back of the house, with \
                               its own door onto the walled garden. A double bed, \
                               a small writing desk, and a shower room. Quiet, \
                               and the easiest room to reach without stairs."
                    .into(),
            },
            Room {
                id: "orchard".into(),
                name: "Orchard Room".into(),
                sleeps: 2,
                rate_pence: 13_000,
                description: "First floor, and yes -- it looks out over the \
                              orchard, which is old and not especially tidy. A \
                              double bed and a bath rather than a shower. The \
                              cheapest of the three, being the smallest."
                    .into(),
            },
            Room {
                id: "loft".into(),
                name: "The Loft".into(),
                sleeps: 4,
                rate_pence: 19_500,
                description: "The whole top floor, under the beams. A double \
                              bed and two singles in a second room, so it takes \
                              a family without anybody sleeping on a sofa. Low \
                              doorways, and a steep staircase that is the reason \
                              it is not suitable for everybody."
                    .into(),
            },
        ],
        bookings: Mutex::new(Vec::new()),
        payment_accounts: vec![
            PaymentAccount {
                id: "pa_4471".into(),
                holder: "J. Okafor".into(),
                label: "Visa ending 4471".into(),
            },
            PaymentAccount {
                id: "pa_8802".into(),
                holder: "R. Sandoval".into(),
                label: "Mastercard ending 8802".into(),
            },
        ],
        charges: Mutex::new(Vec::new()),
        webhook,
        client: reqwest::Client::new(),
    });

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(|| async { "ok" }))
        .route("/openapi.json", get(openapi))
        .route("/rooms", get(list_rooms))
        .route("/rooms/{id}", get(get_room))
        .route("/availability", get(availability))
        .route("/bookings", get(list_bookings).post(create_booking))
        .route("/bookings/{id}", get(get_booking))
        .route("/payment-accounts", get(list_payment_accounts))
        .route("/charges", get(list_charges).post(create_charge))
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

/// One room, with what it is actually like.
///
/// The description is here and not in the list, which is the shape most REST
/// APIs have and the one an agent has to learn to navigate: enough to choose
/// in the list, everything about one here.
async fn get_room(State(state): State<Arc<Fixture>>, Path(id): Path<String>) -> impl IntoResponse {
    match state.rooms.iter().find(|r| r.id == id) {
        Some(room) => (
            StatusCode::OK,
            [(FIXTURE_HEADER, "hollowbrook")],
            Json(RoomDetail {
                id: room.id.clone(),
                name: room.name.clone(),
                sleeps: room.sleeps,
                rate_pence: room.rate_pence,
                description: room.description.clone(),
            }),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [(FIXTURE_HEADER, "hollowbrook")],
            Json(serde_json::json!({ "error": "no such room" })),
        )
            .into_response(),
    }
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

/// The cards the guesthouse holds, by handle.
///
/// No numbers, here or anywhere: see `PaymentAccount`. An agent needs the handle
/// and the label to say "the Visa ending 4471" to a guest, and needs nothing
/// else.
async fn list_payment_accounts(State(state): State<Arc<Fixture>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(FIXTURE_HEADER, "hollowbrook")],
        Json(state.payment_accounts.clone()),
    )
}

async fn list_charges(State(state): State<Arc<Fixture>>) -> impl IntoResponse {
    let charges = state.charges.lock().expect("charges").clone();
    (
        StatusCode::OK,
        [(FIXTURE_HEADER, "hollowbrook")],
        Json(charges),
    )
}

/// Takes money against a card the guesthouse holds.
///
/// The operation the approval demo is about. Nothing here decides whether it is
/// allowed -- that is outturn's business, and a fixture that refused on its own
/// judgement would be testing the fixture. What it does is behave like a
/// recipient that was built properly: it validates, it refuses a repeat that
/// quotes a key it has seen, and it records enough that a test can assert money
/// moved rather than that a turn finished.
async fn create_charge(
    State(state): State<Arc<Fixture>>,
    Json(input): Json<NewCharge>,
) -> impl IntoResponse {
    let refuse = |status: StatusCode, message: &str| {
        (
            status,
            [(FIXTURE_HEADER, "hollowbrook")],
            Json(serde_json::json!({ "error": message })),
        )
            .into_response()
    };

    if !state
        .payment_accounts
        .iter()
        .any(|a| a.id == input.payment_account_id)
    {
        return refuse(StatusCode::NOT_FOUND, "no such payment account");
    }
    if input.amount_pence == 0 {
        return refuse(StatusCode::BAD_REQUEST, "a charge has to be for something");
    }

    let booking_total = {
        let bookings = state.bookings.lock().expect("bookings");
        match bookings.iter().find(|b| b.id == input.booking_id) {
            Some(booking) => booking.total_pence,
            None => return refuse(StatusCode::NOT_FOUND, "no such booking"),
        }
    };
    // A guesthouse does not take more than the stay costs, and an agent that
    // worked the total out for itself is exactly the caller this catches.
    if input.amount_pence > booking_total {
        return refuse(
            StatusCode::BAD_REQUEST,
            "that is more than the booking's total",
        );
    }

    let mut charges = state.charges.lock().expect("charges");

    // A key it has seen returns the first charge rather than taking the money
    // again. Keyed on the charge's own id, since that is what the key was
    // recorded against.
    if let Some(key) = input.idempotency_key.as_deref()
        && let Some(seen) = charges.iter().find(|c| c.id == charge_id(key))
    {
        return (
            StatusCode::OK,
            [(FIXTURE_HEADER, "hollowbrook")],
            Json(seen.clone()),
        )
            .into_response();
    }

    let charge = Charge {
        id: match input.idempotency_key.as_deref() {
            Some(key) => charge_id(key),
            None => format!("ch_{}", &Uuid::now_v7().simple().to_string()[..12]),
        },
        payment_account_id: input.payment_account_id,
        booking_id: input.booking_id,
        amount_pence: input.amount_pence,
        created_at: Utc::now(),
    };
    charges.push(charge.clone());

    (
        StatusCode::CREATED,
        [(FIXTURE_HEADER, "hollowbrook")],
        Json(charge),
    )
        .into_response()
}

/// A charge's id, derived from the caller's idempotency key.
///
/// Derived rather than stored beside it so the lookup above needs no second
/// index and no second field that could disagree. A key is a caller's word for
/// one attempt, and this makes the record of that attempt findable by it.
fn charge_id(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(key.as_bytes());
    format!("ch_{}", hex::encode(&digest[..6]))
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
/// The specification an agent reads before calling any of this.
///
/// Written by hand and kept so deliberately. Axum retains nothing at runtime
/// that a document could be derived from, and the crates that do it
/// (`utoipa` and its kin) derive the schemas from annotations you still write
/// -- so the choice is between annotations near the handlers and a document
/// near nothing. What decided it is that this fixture exists to give the
/// OpenAPI wizard something realistic to read: a specification is the wizard's
/// input, so having one that somebody wrote the way a customer's team writes
/// theirs is the point rather than an accident.
///
/// The response schemas are the part worth maintaining. An agent that knows
/// `total_pence` exists does not ask for a price it was already handed, and a
/// summary saying "The bookings." tells it nothing at all -- which is how the
/// first version of this read.
async fn openapi(State(_state): State<Arc<Fixture>>) -> impl IntoResponse {
    let doc = serde_json::json!({
      "openapi": "3.0.3",
      "info": {
        "title": "Hollowbrook House",
        "version": "1.1.0",
        "description":
          "Rooms and bookings for a small guesthouse. A fixture: nothing here is real.\n\n\
           Money is in pence throughout, so nothing does floating-point arithmetic on it. \
           Dates are YYYY-MM-DD and name nights: `arrival` is the first night and \
           `departure` is the morning the guest leaves, so a stay of one night has a \
           departure one day after its arrival."
      },
      "servers": [
        { "url": "http://outturn-hollowbrook:8084", "description": "Beside outturn in the cluster." }
      ],
      "components": {
        "schemas": {
          "PaymentAccount": {
            "type": "object",
            "required": ["id", "holder", "label"],
            "properties": {
              "id": { "type": "string", "description":
                "A handle for a card the house already holds, e.g. `pa_4471`. Never a card number: quote this to a guest and put it in a record without a second thought." },
              "holder": { "type": "string" },
              "label": { "type": "string", "description":
                "What a guest would recognise, e.g. `Visa ending 4471`." }
            }
          },
          "Charge": {
            "type": "object",
            "required": ["id", "payment_account_id", "booking_id", "amount_pence", "created_at"],
            "properties": {
              "id": { "type": "string" },
              "payment_account_id": { "type": "string" },
              "booking_id": { "type": "string" },
              "amount_pence": { "type": "integer", "description": "Pence." },
              "created_at": { "type": "string", "format": "date-time" }
            }
          },
          "Room": {
            "type": "object",
            "required": ["id", "name", "sleeps", "rate_pence"],
            "properties": {
              "id": { "type": "string", "description": "What a booking names, e.g. `garden`." },
              "name": { "type": "string", "description": "What a person calls it." },
              "sleeps": { "type": "integer", "description": "How many it takes." },
              "rate_pence": { "type": "integer", "description": "One night, in pence." }
            }
          },
          "RoomDetail": {
            "type": "object",
            "description": "A room with what it is actually like, which the list does not carry.",
            "required": ["id", "name", "sleeps", "rate_pence", "description"],
            "properties": {
              "id": { "type": "string" },
              "name": { "type": "string" },
              "sleeps": { "type": "integer" },
              "rate_pence": { "type": "integer", "description": "One night, in pence." },
              "description": {
                "type": "string",
                "description": "Where it is in the house, what is in it, what it overlooks."
              }
            }
          },
          "Booking": {
            "type": "object",
            "required": [
              "id", "room_id", "guest_name", "arrival", "departure",
              "total_pence", "created_at"
            ],
            "properties": {
              "id": { "type": "string", "description": "Quote this to a guest; `getBooking` takes it." },
              "room_id": { "type": "string" },
              "guest_name": { "type": "string" },
              "arrival": { "type": "string", "format": "date" },
              "departure": { "type": "string", "format": "date" },
              "total_pence": {
                "type": "integer",
                "description": "The whole stay, so a caller is not asked to multiply."
              },
              "created_at": { "type": "string", "format": "date-time" }
            }
          },
          "Vacancy": {
            "type": "object",
            "description": "A room free for the dates asked about, priced for that stay.",
            "required": ["room_id", "name", "sleeps", "rate_pence", "total_pence"],
            "properties": {
              "room_id": {
                "type": "string",
                "description": "`room_id` here, not `id` as in Room -- this is what createBooking takes."
              },
              "name": { "type": "string" },
              "sleeps": { "type": "integer" },
              "rate_pence": { "type": "integer", "description": "One night, in pence." },
              "total_pence": { "type": "integer", "description": "Rate times nights." }
            }
          },
          "Error": {
            "type": "object",
            "required": ["error"],
            "properties": { "error": { "type": "string", "description": "What was wrong, in a sentence." } }
          }
        }
      },
      "paths": {
        "/rooms": {
          "get": {
            "operationId": "listRooms",
            "summary": "List every room, with what it sleeps and what it costs a night.",
            "description":
              "Every room the house has, whether or not it is free. Use `checkAvailability` \
               to find out which are free for a stay, and `getRoom` for what one is like -- \
               the description is deliberately not here, so listing the rooms does not carry \
               three of them.",
            "tags": ["rooms"],
            "responses": {
              "200": {
                "description": "The rooms.",
                "content": { "application/json": { "schema": {
                  "type": "object",
                  "required": ["rooms"],
                  "properties": { "rooms": { "type": "array", "items": { "$ref": "#/components/schemas/Room" } } }
                } } }
              }
            }
          }
        },
        "/rooms/{id}": {
          "get": {
            "operationId": "getRoom",
            "summary": "One room, with the full description of what it is like.",
            "description":
              "The description lives here rather than on /rooms, so listing the rooms \
               does not carry three of them. Fetch this when somebody asks what a room \
               is like, not to build a list.",
            "tags": ["rooms"],
            "parameters": [
              { "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }
            ],
            "responses": {
              "200": {
                "description": "The room.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/RoomDetail" } } }
              },
              "404": {
                "description": "No room has that id.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } }
              }
            }
          }
        },
        "/availability": {
          "get": {
            "operationId": "checkAvailability",
            "summary": "Which rooms are free for a stay, and what the stay would cost.",
            "description":
              "A room is free when no booking overlaps the range. The total is the room's \
               nightly rate times the number of nights, so it does not have to be worked out \
               from the rate.",
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
              "200": {
                "description": "Rooms free for those dates. An empty `available` means none are.",
                "content": { "application/json": { "schema": {
                  "type": "object",
                  "required": ["arrival", "departure", "nights", "available"],
                  "properties": {
                    "arrival": { "type": "string", "format": "date" },
                    "departure": { "type": "string", "format": "date" },
                    "nights": { "type": "integer", "description": "What the range works out to." },
                    "available": { "type": "array", "items": { "$ref": "#/components/schemas/Vacancy" } }
                  }
                } } }
              },
              "400": {
                "description": "Departure is not after arrival.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } }
              }
            }
          }
        },
        "/bookings": {
          "get": {
            "operationId": "listBookings",
            "summary": "Every booking currently held.",
            "tags": ["bookings"],
            "responses": {
              "200": {
                "description": "The bookings, oldest first.",
                "content": { "application/json": { "schema": {
                  "type": "object",
                  "required": ["bookings"],
                  "properties": { "bookings": { "type": "array", "items": { "$ref": "#/components/schemas/Booking" } } }
                } } }
              }
            }
          },
          "post": {
            "operationId": "createBooking",
            "summary": "Reserve a room for a guest.",
            "description":
              "The room has to be free for the whole range. A 409 means part of it is taken, \
               and `checkAvailability` for the same dates says what is not.",
            "tags": ["bookings"],
            "requestBody": {
              "required": true,
              "content": {
                "application/json": {
                  "schema": {
                    "type": "object",
                    "required": ["room_id", "guest_name", "arrival", "departure"],
                    "properties": {
                      "room_id": {
                        "type": "string",
                        "description": "A room's id, from listRooms or the `room_id` of a vacancy. Not its name."
                      },
                      "guest_name": { "type": "string" },
                      "arrival": { "type": "string", "format": "date" },
                      "departure": { "type": "string", "format": "date", "description": "Must be after arrival." }
                    }
                  }
                }
              }
            },
            "responses": {
              "201": {
                "description": "The booking that was made, with its id and the total.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Booking" } } }
              },
              "400": {
                "description": "Departure is not after arrival.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } }
              },
              "404": {
                "description": "No room has that id.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } }
              },
              "409": {
                "description": "That room is taken for part of those dates.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } }
              }
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
              "200": {
                "description": "The booking.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Booking" } } }
              },
              "404": {
                "description": "No booking has that id.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } }
              }
            }
          }
        },
        "/payment-accounts": {
          "get": {
            "operationId": "listPaymentAccounts",
            "summary": "The cards the house holds, by handle.",
            "description":
              "Handles and labels, never numbers. Use a handle with `createCharge`, and the \
               label to say which card to a guest.",
            "tags": ["payments"],
            "responses": {
              "200": {
                "description": "The payment accounts.",
                "content": { "application/json": { "schema": {
                  "type": "array", "items": { "$ref": "#/components/schemas/PaymentAccount" }
                } } }
              }
            }
          }
        },
        "/charges": {
          "get": {
            "operationId": "listCharges",
            "summary": "What has been charged.",
            "tags": ["payments"],
            "responses": {
              "200": {
                "description": "The charges.",
                "content": { "application/json": { "schema": {
                  "type": "array", "items": { "$ref": "#/components/schemas/Charge" }
                } } }
              }
            }
          },
          "post": {
            "operationId": "createCharge",
            "summary": "Take money against a card the house holds.",
            "description":
              "Charges a payment account for a booking. Refuses more than the booking's \
               total, so the amount is the stay's cost rather than a figure worked out \
               here. Send `idempotency_key` and a repeat quoting the same key returns the \
               first charge with 200 rather than taking the money twice.",
            "tags": ["payments"],
            "requestBody": {
              "required": true,
              "content": { "application/json": { "schema": {
                "type": "object",
                "required": ["payment_account_id", "booking_id", "amount_pence"],
                "properties": {
                  "payment_account_id": { "type": "string" },
                  "booking_id": { "type": "string" },
                  "amount_pence": { "type": "integer" },
                  "idempotency_key": { "type": "string" }
                }
              } } }
            },
            "responses": {
              "201": {
                "description": "The charge was taken.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Charge" } } }
              },
              "200": {
                "description": "A charge with this idempotency key was already taken; this is that one.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Charge" } } }
              },
              "400": {
                "description": "The amount is zero, or more than the booking's total.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } }
              },
              "404": {
                "description": "No such payment account, or no such booking.",
                "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } }
              }
            }
          }
        }
      }
    });
    marked(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;

    /// Reads the document `openapi` serves, as JSON.
    async fn spec() -> serde_json::Value {
        // Empty: `openapi` ignores its state, and the document is the same
        // whatever the house happens to be holding.
        let state = Arc::new(Fixture {
            rooms: Vec::new(),
            bookings: Mutex::new(Vec::new()),
            payment_accounts: Vec::new(),
            charges: Mutex::new(Vec::new()),
            webhook: None,
            client: reqwest::Client::new(),
        });
        let response = openapi(State(state)).await.into_response();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("the specification must be JSON")
    }

    /// Every route this serves is in the document, and nothing else is.
    ///
    /// The failure this exists for is drift in the direction nobody notices: a
    /// route added and the specification forgotten. An agent reads the
    /// document to decide what it can call, so an operation missing from it is
    /// an operation that does not exist as far as any agent is concerned --
    /// and the wizard that turns this into a skill sees exactly what the
    /// document says.
    ///
    /// Hand-written and hand-maintained, which is the reason to check it
    /// mechanically. A derived document could not drift; this one can, and
    /// only a test will say so.
    #[tokio::test]
    async fn the_specification_describes_every_route_and_no_others() {
        let spec = spec().await;
        let described: std::collections::BTreeSet<String> = spec["paths"]
            .as_object()
            .expect("paths")
            .keys()
            .cloned()
            .collect();

        // The API an agent is given. `/healthz`, `/readyz` and `/openapi.json`
        // are how the cluster and the wizard find their way in rather than
        // things to call, and `/fixture/reset` is named for not being real.
        let served: std::collections::BTreeSet<String> = [
            "/rooms",
            "/rooms/{id}",
            "/availability",
            "/bookings",
            "/bookings/{id}",
            "/payment-accounts",
            "/charges",
        ]
        .into_iter()
        .map(String::from)
        .collect();

        assert_eq!(
            described, served,
            "the specification and the router disagree about what exists"
        );
    }

    /// Every operation says what comes back, not merely that something does.
    ///
    /// A response documented as "The bookings." tells an agent nothing it can
    /// act on: it cannot know a booking carries `total_pence` until it has
    /// made one and looked. That was true of every response here before this
    /// test, which is why it is worth asserting rather than trusting.
    #[tokio::test]
    async fn every_success_response_has_a_schema() {
        let spec = spec().await;
        for (path, methods) in spec["paths"].as_object().expect("paths") {
            for (method, operation) in methods.as_object().expect("methods") {
                let responses = operation["responses"].as_object().expect("responses");
                let (code, response) = responses
                    .iter()
                    .find(|(code, _)| code.starts_with('2'))
                    .unwrap_or_else(|| panic!("{method} {path} describes no success"));
                assert!(
                    response["content"]["application/json"]["schema"].is_object(),
                    "{method} {path} answers {code} with nothing an agent can read"
                );
            }
        }
    }

    /// Every schema a response points at is one the document defines.
    ///
    /// A `$ref` to a name that is not there is the ordinary way a hand-written
    /// document rots, and it fails silently: a reader follows the reference,
    /// finds nothing, and carries on with whatever it already believed.
    #[tokio::test]
    async fn every_reference_resolves() {
        let spec = spec().await;
        let defined: std::collections::BTreeSet<String> = spec["components"]["schemas"]
            .as_object()
            .expect("schemas")
            .keys()
            .cloned()
            .collect();

        let mut referenced = std::collections::BTreeSet::new();
        fn walk(node: &serde_json::Value, found: &mut std::collections::BTreeSet<String>) {
            match node {
                serde_json::Value::Object(map) => {
                    for (key, value) in map {
                        if key == "$ref"
                            && let Some(name) = value.as_str().and_then(|r| {
                                r.strip_prefix("#/components/schemas/").map(String::from)
                            })
                        {
                            found.insert(name);
                        }
                        walk(value, found);
                    }
                }
                serde_json::Value::Array(items) => {
                    for item in items {
                        walk(item, found);
                    }
                }
                _ => {}
            }
        }
        walk(&spec["paths"], &mut referenced);

        assert!(!referenced.is_empty(), "no schema is referenced at all");
        let dangling: Vec<_> = referenced.difference(&defined).collect();
        assert!(
            dangling.is_empty(),
            "referenced but not defined: {dangling:?}"
        );
    }
}

#[cfg(test)]
mod charging {
    use super::*;

    fn fixture() -> Arc<Fixture> {
        Arc::new(Fixture {
            rooms: vec![Room {
                id: "orchard".into(),
                name: "Orchard Room".into(),
                sleeps: 2,
                rate_pence: 13_000,
                description: String::new(),
            }],
            bookings: Mutex::new(vec![Booking {
                id: "bk_test".into(),
                room_id: "orchard".into(),
                guest_name: "J. Okafor".into(),
                arrival: NaiveDate::from_ymd_opt(2026, 10, 2).expect("date"),
                departure: NaiveDate::from_ymd_opt(2026, 10, 4).expect("date"),
                total_pence: 26_000,
                created_at: Utc::now(),
            }]),
            payment_accounts: vec![PaymentAccount {
                id: "pa_4471".into(),
                holder: "J. Okafor".into(),
                label: "Visa ending 4471".into(),
            }],
            charges: Mutex::new(Vec::new()),
            webhook: None,
            client: reqwest::Client::new(),
        })
    }

    async fn charge(
        state: &Arc<Fixture>,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let input: NewCharge = serde_json::from_value(body).expect("a charge request");
        let response = create_charge(State(Arc::clone(state)), Json(input))
            .await
            .into_response();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, serde_json::from_slice(&bytes).expect("json"))
    }

    #[tokio::test]
    async fn a_charge_against_a_known_account_and_booking_is_taken() {
        let state = fixture();
        let (status, body) = charge(
            &state,
            serde_json::json!({
                "payment_account_id": "pa_4471",
                "booking_id": "bk_test",
                "amount_pence": 26_000,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(body["amount_pence"], 26_000);
        assert_eq!(state.charges.lock().expect("charges").len(), 1);
    }

    #[tokio::test]
    async fn more_than_the_bookings_total_is_refused() {
        // The guard that catches an agent working the total out for itself.
        let state = fixture();
        let (status, body) = charge(
            &state,
            serde_json::json!({
                "payment_account_id": "pa_4471",
                "booking_id": "bk_test",
                "amount_pence": 39_000,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(state.charges.lock().expect("charges").is_empty());
    }

    #[tokio::test]
    async fn a_repeat_quoting_the_same_key_takes_the_money_once() {
        // What makes this the recipient docs/idempotency.md calls the best
        // outcome available: a retry is distinguishable from a second charge.
        let state = fixture();
        let request = serde_json::json!({
            "payment_account_id": "pa_4471",
            "booking_id": "bk_test",
            "amount_pence": 13_000,
            "idempotency_key": "turn-7-call-1",
        });

        let (first, first_body) = charge(&state, request.clone()).await;
        assert_eq!(first, StatusCode::CREATED);
        let (second, second_body) = charge(&state, request).await;
        assert_eq!(
            second,
            StatusCode::OK,
            "a repeat should report the first charge rather than create one"
        );
        assert_eq!(first_body["id"], second_body["id"]);
        assert_eq!(
            state.charges.lock().expect("charges").len(),
            1,
            "the money was taken twice"
        );
    }

    #[tokio::test]
    async fn two_different_keys_are_two_charges() {
        // The other half: keying must not collapse deliberately distinct calls,
        // which is the hazard docs/idempotency.md names in key derivation.
        let state = fixture();
        for key in ["turn-7-call-1", "turn-9-call-1"] {
            let (status, body) = charge(
                &state,
                serde_json::json!({
                    "payment_account_id": "pa_4471",
                    "booking_id": "bk_test",
                    "amount_pence": 13_000,
                    "idempotency_key": key,
                }),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED, "{body}");
        }
        assert_eq!(state.charges.lock().expect("charges").len(), 2);
    }

    #[tokio::test]
    async fn an_unknown_account_or_booking_is_refused() {
        let state = fixture();
        let (status, _) = charge(
            &state,
            serde_json::json!({
                "payment_account_id": "pa_nope",
                "booking_id": "bk_test",
                "amount_pence": 1_000,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = charge(
            &state,
            serde_json::json!({
                "payment_account_id": "pa_4471",
                "booking_id": "bk_nope",
                "amount_pence": 1_000,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn nothing_here_can_return_a_card_number() {
        // The reason a payment account number exists. A transcript is replayed
        // to a model on every later turn, so an endpoint that returned a card
        // number would put one there for good.
        let state = fixture();
        let response = list_payment_accounts(State(Arc::clone(&state)))
            .await
            .into_response();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let text = String::from_utf8(bytes.to_vec()).expect("utf8");
        assert!(text.contains("pa_4471"), "{text}");

        // The property, stated as one: nothing on the wire is a run of digits
        // long enough to be a card. `pa_4471` and `4471` are four, and a card is
        // thirteen or more, so the test does not have to know what a card number
        // looks like -- only that there is no field here that could hold one.
        let longest_run = text
            .split(|c: char| !c.is_ascii_digit())
            .map(str::len)
            .max()
            .unwrap_or(0);
        assert!(
            longest_run < 13,
            "a {longest_run}-digit run reached the wire, which is long enough to be a card: {text}"
        );
    }
}
