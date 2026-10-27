/*
 * This file contains 4 functions related to accepting POST-ed data from raspberry pi deployments.
 * 1. run_server() - this reads environment files to configure the database url and the address of the server.
 * 2. build_collector_server() - returns a Router which responds only to '/api/alpr/events'
 * using the handler alpr_webhook_handler().
 * 3. aplr_webhook_handler()-
 * 4. decode_and_save_images_to_files()-
 */
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{Method, StatusCode, header},
    response::IntoResponse,
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::DateTime;
use dotenvy::dotenv;
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set};
use serde::Deserialize;
use std::env;
use std::path::PathBuf;
use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

use crate::db;
use crate::entities::{alpr_events, camera};

/// State for the data collector server.
///
/// This is separate from `handlers::app_state::AppState` (used by the
/// frontend api server) because only the collector writes image files.
#[derive(Clone)]
pub struct CollectorState {
    pub db_conn: DatabaseConnection,
    pub images_dir: PathBuf,
}

#[derive(Debug, Deserialize)]
struct AlprPayload {
    camid: String,
    /// RFC 3339 timestamp, e.g. `2026-10-09T16:44:25.443+08:00`.
    date: String,
    plate: String,
    plate_image: String,
    image: String,
}
//run_server calls build_collector_server to run an http server that passes the database
// as part of the state.
pub async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    // ------------------------
    // 1. Configure the CORS layer
    // ------------------------
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers(Any);

    // ------------------------
    // 2. Read the DATABASE_URL and connect
    // ------------------------
    let db_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set in .env file");
    println!("DATABASE_URL is:{}", db_url);
    let db_conn = db::connect(&db_url).await?;

    // ------------------------
    // 3. Prepare the image output directory
    // ------------------------
    let images_dir = PathBuf::from(env::var("IMAGE_DIR").unwrap_or_else(|_| "images".to_string()));
    std::fs::create_dir_all(&images_dir)?;

    // ------------------------
    // 4. Prepare the state that will be passed to every http request handler.
    // ------------------------
    let state = CollectorState {
        db_conn,
        images_dir,
    };

    // ------------------------
    // 5. Build the app
    // ------------------------
    let app = build_collector_server().layer(cors).with_state(state);

    // The frontend api server uses port 3000, so the collector uses its own.
    let addr = env::var("COLLECTOR_ADDR").unwrap_or_else(|_| "127.0.0.1:3001".to_string());
    println!("Data collector server listening on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    axum::serve(listener, app).await?;

    Ok(())
}

// build_collector_server returns a Router with only one route.
pub fn build_collector_server() -> Router<CollectorState> {
    Router::new().route("/api/alpr/event", post(alpr_webhook_handler))
}

/// Finds the camera that an incoming payload's `camid` refers to.
///
/// NOTE: currently `camid` is matched against `camera.id`. If the mapping
/// between the device-reported id and the camera record is different, this is
/// the only place that needs to change.
async fn find_camera(
    db: &DatabaseConnection,
    camid: &str,
) -> Result<Option<camera::Model>, sea_orm::DbErr> {
    camera::Entity::find_by_id(camid.to_string()).one(db).await
}

// handler for a POST-ed event. The event is a json relayed by the raspi from the camera.
// The structure is expected to be similar to the AlprPayload, differing only in camid.
async fn alpr_webhook_handler(
    State(state): State<CollectorState>,
    _headers: header::HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    println!("\n=== Incoming ALPR Trigger from PI ===");

    // ---------------------------
    // 1. Decode the JSON payload.
    // ---------------------------
    let payload: AlprPayload = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(error) => {
            eprintln!("Failed to parse ALPR JSON: {}", error);
            return (StatusCode::BAD_REQUEST, "Invalid JSON payload".to_string());
        }
    };

    println!("Camera: {}", payload.camid);
    println!("Plate:  {}", payload.plate);
    println!("Date:   {}", payload.date);

    // ---------------------------
    // 2. Parse the event date.
    // ---------------------------
    let event_date = match DateTime::parse_from_rfc3339(&payload.date) {
        Ok(date) => date,
        Err(error) => {
            eprintln!("Failed to parse date '{}': {}", payload.date, error);
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid date '{}': expected RFC 3339", payload.date),
            );
        }
    };

    // ---------------------------------------------------------
    // 3. Make sure the camera is registered, before doing any work.
    // ---------------------------------------------------------
    let camera = match find_camera(&state.db_conn, &payload.camid).await {
        Ok(Some(camera)) => camera,
        Ok(None) => {
            eprintln!("Rejected event: unknown camera '{}'", payload.camid);
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("Unknown camera '{}'", payload.camid),
            );
        }
        Err(error) => {
            eprintln!("Failed to look up camera '{}': {}", payload.camid, error);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database error while looking up camera".to_string(),
            );
        }
    };

    // --------------------------------------
    // 4. Generate a unique ID for this event
    // --------------------------------------
    let event_id = Uuid::new_v4().to_string();

    // ----------------------------------------------
    // 5. Decode and save the event images into files.
    // ----------------------------------------------
    let (image_filename, plate_filename) = match decode_and_save_images_to_files(
        &state,
        &event_id,
        &payload.image,
        &payload.plate_image,
    ) {
        Ok(filenames) => filenames,
        Err(error) => {
            eprintln!("Failed to save images: {}", error);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to save images".to_string(),
            );
        }
    };

    // ------------------------------------
    // 6. Insert the event using SeaORM
    // ------------------------------------
    let new_event = alpr_events::ActiveModel {
        id: Set(event_id),
        camera_id: Set(camera.id),
        date: Set(event_date),
        plate: Set(payload.plate),
        image: Set(image_filename.clone()),
        plate_image: Set(plate_filename.clone()),
    };

    if let Err(error) = new_event.insert(&state.db_conn).await {
        eprintln!("Failed to insert alpr event: {}", error);
        // Don't leave orphaned image files behind.
        let _ = std::fs::remove_file(state.images_dir.join(&image_filename));
        let _ = std::fs::remove_file(state.images_dir.join(&plate_filename));
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to save event".to_string(),
        );
    }

    (StatusCode::OK, "OK".to_string())
}

fn decode_and_save_images_to_files(
    state: &CollectorState,
    event_id: &str,
    payload_image: &str,
    plate_image: &str,
) -> Result<(String, String), std::io::Error> {
    // ------------------------
    // 1. Decode the scene image.
    // -------------------------
    let image_bytes = STANDARD.decode(payload_image).map_err(|error| {
        eprintln!("Failed to decode scene image: {}", error);
        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
    })?;

    // ------------------------
    // 2. Decode the plate image.
    // ------------------------
    let plate_image_bytes = STANDARD.decode(plate_image).map_err(|error| {
        eprintln!("Failed to decode plate image: {}", error);
        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
    })?;

    //------------------
    // 3. Set the relevant variables.
    //-------------------
    let image_filename = format!("event_{}.jpg", event_id);
    let plate_image_filename = format!("event_{}_plate.jpg", event_id);

    let image_path = state.images_dir.join(&image_filename);
    let plate_image_path = state.images_dir.join(&plate_image_filename);

    //------------------
    // 4. Save scene image.
    //-------------------
    std::fs::write(&image_path, image_bytes).map_err(|error| {
        eprintln!("Failed to save scene image: {}", error);
        error
    })?;

    // -----------------
    // 5. Save plate image.
    //------------------
    if let Err(error) = std::fs::write(&plate_image_path, plate_image_bytes) {
        eprintln!("Failed to save plate image: {}", error);

        // Don't leave an orphaned scene image.
        let _ = std::fs::remove_file(&image_path);

        return Err(error);
    }

    Ok((image_filename, plate_image_filename))
}
