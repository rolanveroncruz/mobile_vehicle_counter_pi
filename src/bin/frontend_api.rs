use mobile_vehicle_counter_server::api_server;
use tokio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    api_server::run_server().await?;
    Ok(())
}
