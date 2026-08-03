/// Enumerate clock sources as part of the board
pub fn enumerate_clocks() {
    #[cfg(target_arch = "x86_64")]
    super::pc::rtc::init_realtime_clock();
}
