use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};
use serde::Deserialize;
use uuid::Uuid;

use crate::entities::sublocation; // Adjust if your entity module path is different
use crate::handlers::app_state::AppState;

#[derive(Deserialize)]
pub struct CreateSublocationRequest {
    pub name: String,
    pub description: Option<String>,
    pub location_id: String,
}

/// GET /api/locations/:location_id/sublocations
pub async fn get_sublocations_by_location(
    State(state): State<AppState>,
    Path(location_id): Path<String>,
) -> Result<Json<Vec<sublocation::Model>>, StatusCode> {
    let sublocations = sublocation::Entity::find()
        .filter(sublocation::Column::LocationId.eq(location_id))
        .all(&state.db_conn)
        .await
        .map_err(|error| {
            eprintln!("Failed to fetch sublocations: {}", error);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(sublocations))
}

/// POST /api/sublocations
pub async fn create_sublocation(
    State(state): State<AppState>,
    Json(payload): Json<CreateSublocationRequest>,
) -> Result<(StatusCode, Json<sublocation::Model>), StatusCode> {
    // Generate a new UUID string for the primary key
    let new_uuid = Uuid::new_v4().to_string();

    // Construct the active model for insertion
    let new_sublocation = sublocation::ActiveModel {
        id: Set(new_uuid),
        name: Set(payload.name),
        description: Set(payload.description),
        location_id: Set(payload.location_id),
        ..Default::default()
    };

    // Insert and immediately return the created model
    let created = new_sublocation.insert(&state.db_conn).await.map_err(|error| {
        eprintln!("Failed to create sublocation: {}", error);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// DELETE /api/sublocations/:id
pub async fn delete_sublocation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let result = sublocation::Entity::delete_by_id(id)
        .exec(&state.db_conn)
        .await
        .map_err(|error| {
            eprintln!("Failed to delete sublocation: {}", error);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    if result.rows_affected == 0 {
        return Err(StatusCode::NOT_FOUND); // Return 404 if the ID didn't exist
    }

    Ok(StatusCode::NO_CONTENT) // Return 204 No Content on successful deletion
}