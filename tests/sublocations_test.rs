use axum::{body::Body, http::{Request, header, StatusCode}, Router};
use tower::ServiceExt;
use serde_json::{json, Value};
use sea_orm::Database;
use std::env;
use dotenvy::dotenv;

// Import from your library crate
use mobile_vehicle_counter_server::handlers::app_state::AppState;
use mobile_vehicle_counter_server::server::build_app;
use mobile_vehicle_counter_server::entities::location::Model as LocationModel;
use mobile_vehicle_counter_server::entities::sublocation::Model as SublocationModel;

#[tokio::test]
async fn test_full_sublocation_lifecycle() {
    // 1. Setup environment and DB
    dotenv().ok();
    let db_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let _jwt_secret = env::var("JWT_SECRET").expect("JWT_SECRET must be set");

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
    assert_eq!(res.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let auth_response: Value = serde_json::from_slice(&body_bytes).unwrap();
    let token = auth_response["access_token"].as_str().unwrap().to_string();
    let auth_header = format!("Bearer {}", token);

    // ==========================================
    // STEP 2: CREATE PARENT LOCATION
    // ==========================================
    let parent_location = post_location_with_description(
        "Sublocation Integration Test Parent",
        "Parent Location for Sublocation Tests",
        app.clone(),
        &auth_header
    ).await;

    // ==========================================
    // STEP 3: CREATE SUBLOCATIONS
    // ==========================================
    let mut created_sublocations: Vec<SublocationModel> = Vec::new();

    for i in 0..3 {
        let name = format!("Integration Test Sublocation {}", i);
        let description = format!("Integration Test Sublocation Desc {}", i);
        let new_sublocation = post_sublocation(
            &name,
            Some(&description),
            &parent_location.id,
            app.clone(),
            &auth_header
        ).await;
        created_sublocations.push(new_sublocation);
    }

    // ==========================================
    // STEP 4: GET SUBLOCATIONS & VERIFY EXISTENCE
    // ==========================================
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/locations/{}/sublocations", parent_location.id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let fetched_sublocations: Vec<SublocationModel> = serde_json::from_slice(&body_bytes).unwrap();

    for expected in &created_sublocations {
        let exists = fetched_sublocations.iter().any(|sub| sub.id == expected.id && sub.name == expected.name);
        assert!(exists, "Newly created sublocation was not found in the GET array");
    }

    // ==========================================
    // STEP 5: DELETE THE SUBLOCATIONS
    // ==========================================
    for sub_to_delete in &created_sublocations {
        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/api/sublocations/{}", sub_to_delete.id))
            .header(header::AUTHORIZATION, &auth_header)
            .body(Body::empty())
            .unwrap();

        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    // ==========================================
    // STEP 6: GET SUBLOCATIONS & VERIFY DELETION
    // ==========================================
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/locations/{}/sublocations", parent_location.id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let sublocations_after: Vec<SublocationModel> = serde_json::from_slice(&body_bytes).unwrap();

    for deleted in created_sublocations {
        let still_exists = sublocations_after.iter().any(|sub| sub.id == deleted.id && sub.name == deleted.name);
        assert!(!still_exists, "Sublocation was found in GET array after deletion");
    }

    // ==========================================
    // STEP 7: CLEANUP PARENT LOCATION
    // ==========================================
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/locations/{}", parent_location.id))
        .header(header::AUTHORIZATION, &auth_header)
        .body(Body::empty())
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

// ------------------------------------------------------------------
// HELPER FUNCTIONS
// ------------------------------------------------------------------

async fn post_location_with_description(name: &str, description: &str, app: Router, auth_header: &str) -> LocationModel {
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
    serde_json::from_slice(&body_bytes).unwrap()
}

async fn post_sublocation(name: &str, description: Option<&str>, location_id: &str, app: Router, auth_header: &str) -> SublocationModel {
    let create_req = json!({
        "name": name,
        "description": description,
        "location_id": location_id
    });

    let req = Request::builder()
        .method("POST")
        .uri("/api/sublocations")
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, auth_header)
        .body(Body::from(serde_json::to_vec(&create_req).unwrap()))
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&body_bytes).unwrap()
}