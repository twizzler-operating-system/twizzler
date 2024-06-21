#![feature(naked_functions)]
#![feature(thread_local)]

#[thread_local]
pub static mut FOO: u32 = 32;

pub fn main() {
    println!("Hello World! Binary support! {}", unsafe { FOO });
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_max_level(tracing::Level::TRACE)
        .with_target(false)
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::ACTIVE)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    std::env::set_var("RUST_BACKTRACE", "1");

    tracing::info!("doing secure gate test");
    let res = logboi::logboi_test(3);
    tracing::info!("==> {:?}", res);
}

/*
#[secgate::secure_gate(options(info))]
#[no_mangle]
pub fn bar(info: &GateCallInfo, _x: u32, _y: bool) -> u32 {
    420
}
*/

#[used]
#[doc(hidden)]
#[allow(non_upper_case_globals)]
#[link_section = ".init_array"]
static ___cons_test___ctor: unsafe extern "C" fn() = {
    #[allow(non_snake_case)]
    #[link_section = ".text.startup"]
    unsafe extern "C" fn ___cons_test___ctor() {
        cons_test()
    }
    ___cons_test___ctor
};
unsafe fn cons_test() {
    println!("CONS TEST");
}
