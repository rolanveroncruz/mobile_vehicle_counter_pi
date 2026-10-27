#![allow(dead_code, unused)]
use dotenvy::dotenv;
use std::env;

use axum::http::Method;
use axum::routing::delete;
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{StatusCode, header},
    middleware,
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sea_orm::{Database, Iden};
use serde::{Deserialize, Serialize};

use tower_http::cors::{Any, CorsLayer};
use uuid::Uuid;

use crate::handlers::app_state::AppState;
use crate::handlers::events::{get_bin_event_details, get_binned_events, get_time_limits};
use crate::handlers::locations;
use crate::handlers::locations::{create_location, delete_location, get_all_locations};
use crate::handlers::login::login_handler;
use crate::handlers::middleware::auth_middleware;
use crate::handlers::sublocations::{
    create_sublocation, delete_sublocation, get_sublocations_by_location,
};

pub fn build_app(state: AppState) -> Router<AppState> {
    // -----------------------
    // 3. Setup axum server
    // -----------------------
    let public_routes = Router::new().route("/api/login", post(login_handler));

    let protected_routes = Router::<AppState>::new()
        .route(
            "/api/locations",
            get(get_all_locations).post(create_location),
        )
        .route("/api/locations/{id}", delete(delete_location))
        .route("/api/time_limits", post(get_time_limits))
        .route("/api/binned_events", post(get_binned_events))
        .route("/api/bin_events_details", post(get_bin_event_details))
        .route(
            "/api/locations/{location_id}/sublocations",
            get(get_sublocations_by_location),
        )
        .route("/api/sublocations", post(create_sublocation))
        .route("/api/sublocations/{id}", delete(delete_sublocation))
        .route_layer(middleware::from_fn(auth_middleware));

    let app = Router::new().merge(public_routes).merge(protected_routes);

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

    let state = AppState { db_conn };

    let app = build_app(state.clone()).layer(cors).with_state(state);

    println!("Alarm server listening on http://127.0.0.1:3000");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;

    axum::serve(listener, app).await?;

    Ok(())
}
