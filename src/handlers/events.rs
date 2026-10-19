use axum::{extract::State, Json};
use chrono::{DateTime, Datelike, Duration, FixedOffset, TimeZone, Timelike};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, };
use serde::{Deserialize, Serialize};

// Assuming these are imported from your entity modules
use crate::entities::{alpr_events, camera};
use crate::handlers::app_state::AppState;

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
        .filter(alpr_events::Column::Date.gte(payload.start_datetime.naive_local()))
        .filter(alpr_events::Column::Date.lt(payload.end_datetime.naive_local()))
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
        if let Some(bin) = bins.iter_mut().find(|b| {
            event.date >= b.start_time.naive_local() && event.date < b.end_time.naive_local()
        }) {
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
                    current.second()
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