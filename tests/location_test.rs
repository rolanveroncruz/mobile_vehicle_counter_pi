use axum::{body::Body, http::{Request, header, StatusCode}, Router};
use tower::ServiceExt;
use serde_json::{json, Value};
use sea_orm::Database;
use std::env;
use dotenvy::dotenv;

// Import from your library crate
use mobile_vehicle_counter_server::handlers::app_state::AppState;
use mobile_vehicle_counter_server::api_server::build_app;
use mobile_vehicle_counter_server::entities::location::Model as LocationModel;
#[tokio::test]
async fn test_full_location_lifecycle() {
    // 1. Setup environment and DB
    dotenv().ok();
    let db_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let _jwt_secret= env::var("JWT_SECRET").expect("JWT_SECRET must be set");
    println!("DB URL: {}", db_url);
    println!("JWT_SECRET: {}",_jwt_secret);
    let db_conn = Database::connect(&db_url).await.unwrap();
    let state = AppState { db_conn };

    // 2. Build the app using your production router configuration
    let app = build_app(state.clone()).with_state(state);

    // ==========================================
    // STEP 1: LOGIN TO GET TOKEN
    // ==========================================
    let login_req = json!({
        "email": "admin@admin",
        "password": "asdf"
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&login_req).unwrap()))
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK, "The values of res.status()={} and StatusCode::OK={} are not equal ", res.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let auth_response: Value = serde_json::from_slice(&body_bytes).unwrap();
    let token = auth_response["access_token"].as_str().unwrap().to_string();
    let auth_header = format!("Bearer {}", token);

    // ==========================================
    // STEP 2: CREATE LOCATIONS
    // ==========================================
    let mut created_locations:Vec<LocationModel> = Vec::new();

    for i in 0..5 {
        let location_name = format!("Integration Test Location {}", i);
        let location_description = format!("Integration Test Location Description {}", i);
        let new_location = post_location_with_description(&location_name, &location_description, app.clone(), &auth_header). await;
        created_locations.push(new_location);
    }

    // ==========================================
    // STEP 3: GET LOCATIONS & VERIFY EXISTENCE
    // ==========================================
    let req = Request::builder()
        .method("GET")
        .uri("/api/locations")
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let fetched_locations: Vec<LocationModel> = serde_json::from_slice(&body_bytes).unwrap();

    for expected in &created_locations {
        let exists = fetched_locations.iter().any(|loc| loc.id == expected.id && loc.name == expected.name);
        assert!(exists, "Newly created location was not found in the GET array");
    }

    // ==========================================
    // STEP 4: DELETE THE LOCATIONS
    // ==========================================
    for loc_to_delete in &created_locations{
        let req = Request::builder()
            .method("DELETE")
            // Note: Your router is configured as /api/locations/{id}[cite: 6]
            // Ensure Axum 0.8 correctly maps this, otherwise change it to /api/locations/:id
            .uri(format!("/api/locations/{}", loc_to_delete.id))
            .header(header::AUTHORIZATION, &auth_header)
            .body(Body::empty())
            .unwrap();

        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }


    // ==========================================
    // STEP 5: GET LOCATIONS & VERIFY DELETION
    // ==========================================
    let req = Request::builder()
        .method("GET")
        .uri("/api/locations")
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let locations_after: Vec<LocationModel> = serde_json::from_slice(&body_bytes).unwrap();

    for deleted in created_locations{
        let still_exists = locations_after.iter().any(|loc| loc.id == deleted.id && loc.name == deleted.name);
        assert!(!still_exists, "Location was found in GET array after deletion");
    }
}

async fn post_location_with_description(name: &str, description: &str, app:Router, auth_header: &str)->LocationModel {
    let create_req = json!({
        "name": name,
        "description": description
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/locations")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, auth_header)
        .body(Body::from(serde_json::to_vec(&create_req).unwrap()))
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let created_location: LocationModel = serde_json::from_slice(&body_bytes).unwrap();

    created_location


}