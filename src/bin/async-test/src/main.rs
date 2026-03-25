use std::time::Duration;

#[tokio::main]
async fn main() {
    println!("Hello, world!");
    tokio::time::sleep(Duration::from_secs(1)).await;
    println!("sleep returned!");
}
