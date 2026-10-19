#![allow(dead_code,unused)]
use dotenvy::dotenv;
use std::env;

use axum::{
    body::Bytes,
    extract::State,
    http::{header, StatusCode},
    middleware,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use axum::http::Method;
use axum::routing::delete;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use sea_orm::{Database, Iden};

use uuid::Uuid;
use tower_http::cors::{Any, CorsLayer};

use crate::handlers::app_state::AppState;
use crate::handlers::locations;
use crate::handlers::locations::{create_location, delete_location, get_all_locations};
use crate::handlers::login::login_handler;
use crate::handlers::middleware::auth_middleware;
use crate::handlers::events::get_binned_events;
use crate::handlers::sublocations::{create_sublocation, delete_sublocation, get_sublocations_by_location};

pub fn build_app(state: AppState)->Router<AppState>{

    // -----------------------
    // 3. Setup axum server
    // -----------------------
    let public_routes= Router::new()
        .route( "/api/login", post(login_handler));

    let protected_routes = Router::<AppState>::new()
        .route("/api/locations", get(get_all_locations).post(create_location))
        .route("/api/locations/{id}", delete(delete_location))
        .route("/api/binned_events", post(get_binned_events))
        .route("/api/locations/{location_id}/sublocations", get(get_sublocations_by_location))
        .route("/api/sublocations", post(create_sublocation))
        .route("/api/sublocations/{id}", delete(delete_sublocation))
        .route_layer(middleware::from_fn(auth_middleware));

    let app = Router::new()
        .merge(public_routes)
        .merge(protected_routes);

    app

}
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
    let db_conn = Database::connect(&db_url).await?;

    let state = AppState {
        db_conn,
    };

    let app = build_app(state.clone())
        .layer(cors)
        .with_state(state);

    println!("Alarm server listening on http://127.0.0.1:3000");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;

    axum::serve(listener, app).await?;

    Ok(())
}



/// Receive an ALPR event from the camera.
async fn alpr_webhook_handler(
    State(state): State<AppState>,
    _headers: header::HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
}

fn decode_and_save_images_to_files(state:&AppState,
                                   event_id: &str,
                                   payload_image: &str,
                                   plate_image:  &str,
)->Result<(String, String), std::io::Error>{
    // ------------------------
    // 1. Decode the scene image.
    // -------------------------
/*
    let image_bytes = match STANDARD.decode(payload_image) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("Failed to decode scene image: {}", error);
            return Err(std::io::Error::new(std::io::ErrorKind::Other, error));

        }
    };

    // ------------------------
    // 2.Decode the plate image.
    // ------------------------
    let plate_image_bytes = match STANDARD.decode(plate_image) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("Failed to decode plate image: {}", error);
            return Err(std::io::Error::new(std::io::ErrorKind::Other, error));
        }
    };

    //------------------
    // 3. Set the relevant variables.
    //-------------------
    let image_filename = format!("event_{}.jpg", event_id);
    let plate_image_filename = format!("event_{}_plate.jpg", event_id);

    let image_path = state.out_dir.join(&image_filename);
    let plate_image_path = state.out_dir.join(&plate_image_filename);

    //------------------
    // 4. Save scene image.
    //-------------------
    if let Err(error) = std::fs::write(&image_path, image_bytes) {
        eprintln!("Failed to save scene image: {}", error);
        return Err(std::io::Error::new(std::io::ErrorKind::Other, error));
    }

    // -----------------
    // 5. Save plate image.
    //------------------
    if let Err(error) = std::fs::write(&plate_image_path, plate_image_bytes) {
        eprintln!("Failed to save plate image: {}", error);

        // ✅ Don't leave an orphaned scene image.
        let _ = std::fs::remove_file(&image_path);

        return Err(std::io::Error::new(std::io::ErrorKind::Other, error));
    }
    Ok((image_filename, plate_image_filename))
*/
    Ok(("hello".to_string(), "world".to_string()))
}

#[derive(Debug, Serialize)]
struct AlprRecord {
    id: String,
    camid: String,
    date: String,
    plate: String,
    image: String,
    plate_image: String,
}
/// Return all ALPR events as a JSON array.
async fn get_alpr_events(
    State(state): State<AppState>,
) -> Result<Json<Vec<AlprRecord>>, StatusCode> {
    Ok(Json(vec![]))
}