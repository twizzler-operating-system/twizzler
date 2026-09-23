/// Enumerate clock sources as part of the board
pub fn enumerate_clocks() {
    #[cfg(target_arch = "aarch64")]
    super::arm::machine_clocks();
}
