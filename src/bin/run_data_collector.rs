use mobile_vehicle_counter_server::collector_server;
use tokio;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    collector_server::run_server().await?;
    Ok(())
}
