extern "C" {
    fn std_runtime_start();
}

/* This is essentially just a hook to get us out of the arch-specific code before calling into std.
 * I don't know if we have a panic runtime yet, so I'm not going to try doing a catch panic kind of
 * deal. Instead, we expect the runtime start function to return an exit code, and we'll deal with
 * exiting the thread using that code.
 */
pub(crate) extern "C" fn twz_runtime_start() -> ! {
    /* it's unsafe because it's an extern C function. */
    /* TODO: pass env and args */
    unsafe { std_runtime_start() };
    /* TODO: exit thread */
    loop {}
}
