use secgate::GateCallInfo;

#[secgate::secure_gate(options(info))]
fn bar(info: &GateCallInfo, x: i32, y: bool) -> u32 {
    tracing::info!("in sec gate bar: {} {}: {:?}", x, y, info);
    42
}
