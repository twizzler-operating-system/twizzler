#[secgate::secure_gate]
fn bar(x: i32, y: bool) -> u32 {
    tracing::info!("in sec gate bar: {} {}", x, y);
    420
}
