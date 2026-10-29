use axum::{Json, extract::State};
use chrono::{DateTime, Datelike, Duration, FixedOffset, TimeZone, Timelike};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};
use serde::{Deserialize, Serialize};

// Assuming these are imported from your entity modules
use crate::entities::{alpr_events, camera};
use crate::handlers::app_state::AppState;
use crate::time::to_storage_offset;

// region: Get_time_limits
#[derive(Deserialize, Debug)]
pub struct TimeLimitsRequest {
    pub sublocation_ids: Vec<String>,
}

#[derive(Serialize, Debug)]
pub struct TimeLimitsResponse {
    pub earliest_datetime: Option<DateTime<FixedOffset>>,
    pub latest_datetime: Option<DateTime<FixedOffset>>,
}

/// POST /api/time_limits
pub async fn get_time_limits(
    State(state): State<AppState>,
    Json(payload): Json<TimeLimitsRequest>,
) -> Result<Json<TimeLimitsResponse>, axum::http::StatusCode> {
    // Fetch the earliest event
    let earliest_event = alpr_events::Entity::find()
        .inner_join(camera::Entity)
        .filter(camera::Column::SublocationId.is_in(payload.sublocation_ids.clone()))
        .order_by_asc(alpr_events::Column::Date)
        .one(&state.db_conn)
        .await
        .map_err(|error| {
            eprintln!("Failed to fetch earliest time limit: {}", error);
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Fetch the latest event
    let latest_event = alpr_events::Entity::find()
        .inner_join(camera::Entity)
        .filter(camera::Column::SublocationId.is_in(payload.sublocation_ids))
        .order_by_desc(alpr_events::Column::Date)
        .one(&state.db_conn)
        .await
        .map_err(|error| {
            eprintln!("Failed to fetch latest time limit: {}", error);
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Map the fetched records to their dates. If no events exist, they return None.
    let response = TimeLimitsResponse {
        earliest_datetime: earliest_event.map(|e| e.date),
        latest_datetime: latest_event.map(|e| e.date),
    };

    Ok(Json(response))
}

// endregion: Get_time_limits

// region: Get_binned_events
#[derive(Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum BinInterval {
    Minutes,
    Hours,
    Days,
    Weeks,
    Months,
}

#[derive(Deserialize, Debug)]
pub struct EventBinRequest {
    pub sublocation_ids: Vec<String>,
    pub start_datetime: DateTime<FixedOffset>,
    pub end_datetime: DateTime<FixedOffset>,
    pub interval: BinInterval,
}

#[derive(Serialize, Debug)]
pub struct BinResponse {
    pub start_time: DateTime<FixedOffset>,
    pub end_time: DateTime<FixedOffset>,
    pub count: usize,
}

pub async fn get_binned_events(
    State(state): State<AppState>,
    Json(payload): Json<EventBinRequest>,
) -> Result<Json<Vec<BinResponse>>, axum::http::StatusCode> {
    // 1. Fetch all events within the overall time range for the specified sublocations
    // We join the camera to filter by sublocation_id.
    let events = alpr_events::Entity::find()
        .inner_join(camera::Entity)
        .filter(camera::Column::SublocationId.is_in(payload.sublocation_ids))
        // SQLite compares the stored date as text, so the bounds must use the
        // same offset as the stored values. See `time.rs`.
        .filter(alpr_events::Column::Date.gte(to_storage_offset(payload.start_datetime)))
        .filter(alpr_events::Column::Date.lt(to_storage_offset(payload.end_datetime)))
        .all(&state.db_conn)
        .await
        .map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)?;

    // 2. Generate the empty bins
    let mut bins = Vec::new();
    let mut current_start = payload.start_datetime;

    while current_start < payload.end_datetime {
        let next_start = advance_time(&current_start, &payload.interval);

        // Ensure the last bin truncates exactly at the requested end_datetime
        let actual_end = if next_start > payload.end_datetime {
            payload.end_datetime
        } else {
            next_start
        };

        bins.push(BinResponse {
            start_time: current_start,
            end_time: actual_end,
            count: 0,
        });

        current_start = next_start;
    }

    // 3. Assign events to their respective bins
    // Since dates are stored with timezones (e.g., +08:00), chrono handles the comparisons properly.
    for event in events {
        // Find the bin where: bin.start_time <= event.date < bin.end_time
        if let Some(bin) = bins
            .iter_mut()
            .find(|b| event.date >= b.start_time && event.date < b.end_time)
        {
            bin.count += 1;
        }
    }

    Ok(Json(bins))
}

/// Helper function to advance a DateTime based on the requested interval.
/// Handles standard duration additions, and custom logic for months since
/// months vary in length.
fn advance_time(current: &DateTime<FixedOffset>, interval: &BinInterval) -> DateTime<FixedOffset> {
    match interval {
        BinInterval::Minutes => *current + Duration::minutes(1),
        BinInterval::Hours => *current + Duration::hours(1),
        BinInterval::Days => *current + Duration::days(1),
        BinInterval::Weeks => *current + Duration::weeks(1),
        BinInterval::Months => {
            let mut year = current.year();
            let mut month = current.month() + 1;

            if month > 12 {
                month = 1;
                year += 1;
            }

            // Handle edge case where the next month has fewer days (e.g., Jan 31 -> Feb)
            // If the day is invalid for the new month, it falls back to the max valid day.
            let mut day = current.day();
            loop {
                if let chrono::LocalResult::Single(dt) = current.timezone().with_ymd_and_hms(
                    year,
                    month,
                    day,
                    current.hour(),
                    current.minute(),
                    current.second(),
                ) {
                    // Re-apply the exact nanoseconds for millisecond precision matching
                    return dt.with_nanosecond(current.nanosecond()).unwrap_or(dt);
                }
                day -= 1; // Decrement day until valid (e.g., 31 -> 30 -> 29 -> 28)
                if day == 0 {
                    panic!("Failed to calculate next month");
                }
            }
        }
    }
}
// endregion: Get_binned_events

// region: Get_binned_events
#[derive(Deserialize, Debug)]
pub struct EventDetailsRequest {
    pub sublocation_ids: Vec<String>,
    pub start_datetime: DateTime<FixedOffset>,
    pub end_datetime: DateTime<FixedOffset>,
}

pub async fn get_bin_event_details(
    State(state): State<AppState>,
    Json(payload): Json<EventDetailsRequest>,
) -> Result<Json<Vec<alpr_events::Model>>, axum::http::StatusCode> {
    let events = alpr_events::Entity::find()
        .inner_join(camera::Entity)
        .filter(camera::Column::SublocationId.is_in(payload.sublocation_ids))
        // Bounds use the storage offset; see `time.rs`.
        .filter(alpr_events::Column::Date.gte(to_storage_offset(payload.start_datetime)))
        .filter(alpr_events::Column::Date.lt(to_storage_offset(payload.end_datetime)))
        .order_by_desc(alpr_events::Column::Date) // Optional: returns newest events first
        .all(&state.db_conn)
        .await
        .map_err(|error| {
            eprintln!("Failed to fetch event details: {}", error);
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        })?;

    Ok(Json(events))
}

// endregion: Get_binned_events
