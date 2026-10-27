use mobile_vehicle_counter_server::handlers::app_state::AppState;

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

    let app = build_collector_server(state.clone())
        .layer(cors)
        .with_state(state);

    println!("Alarm server listening on http://127.0.0.1:3000");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;

    axum::serve(listener, app).await?;

    Ok(())
}

fn build_collector_sever(state: AppState) -> Router<AppState> {
    let public_routes = Router::new().route("/api/alpr/event", post(alpr_webhook_handler));
}

async fn alpr_webhook_handler(
    State(state): State<AppState>,
    _headers: header::HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    println!("\n=== Incoming ALPR Trigger from VIGI ===");

    // ---------------------------
    // 1. Decode the JSON payload.
    // ---------------------------

    let payload: AlprPayload = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(error) => {
            eprintln!("Failed to parse ALPR JSON: {}", error);
            return StatusCode::BAD_REQUEST;
        }
    };

    println!("Camera: {}", payload.camid);
    println!("Plate:  {}", payload.plate);
    println!("Date:   {}", payload.date);

    // ---------------------------------
    // 2. Open a connection with sqlite
    // ---------------------------------
    let connection = match Connection::open(&state.db_path) {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("Failed to open database: {}", error);
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    // --------------------------------------
    // 3. Generate a unique ID for this event
    // --------------------------------------
    let event_id = Uuid::new_v4().to_string();

    // ----------------------------------------------
    // 4. Decode and save the event images into files.
    // ----------------------------------------------

    let (image_filename, plate_filename) = match decode_and_save_images_to_files(
        &state,
        &event_id,
        &payload.image,
        &payload.plate_image,
    ) {
        Ok(filenames) => filenames,
        Err(error) => {
            eprintln!("Failed to save  images:{}", error);
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    // ------------------------------------
    // 5. Insert the event into SQLite
    // ------------------------------------
    let result = connection.execute(
        r#"
        INSERT INTO alpr_events
            (id,camid, date, plate, image, plate_image)
        VALUES
            (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
        params![
            event_id,
            payload.camid,
            payload.date,
            payload.plate,
            image_filename,
            plate_filename,
        ],
    );
    if let Err(error) = result {
        eprintln!("Failed to insert alpr event: {}", error);
        let _ = std::fs::remove_file(state.out_dir.join(&image_filename));
        let _ = std::fs::remove_file(state.out_dir.join(&plate_filename));
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    StatusCode::OK
}

fn decode_and_save_images_to_files(
    state: &AppState,
    event_id: &str,
    payload_image: &str,
    plate_image: &str,
) -> Result<(String, String), std::io::Error> {
    // ------------------------
    // 1. Decode the scene image.
    // -------------------------

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
}
