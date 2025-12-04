# Secure Gates

Secure gates enable a thread to cross between programs, calling a function
in one program from another. More specifically, it allows threads to cross
security context boundaries by defining particular entry points in the callee
context that can be called from other contexts. The entry points are typically
defined in a crate as functions to be called, and then exposed via a library.

For example, consider the pager and the pager-srv crates. The pager crate
defines a number of types and an API for calling the pager. The pager-srv crate
contains the actual (long-running) pager code. Applications can then use the pager
crate to call into that code. The pager crate, for example, provides a `struct PagerStats` and a gate function `pager_get_stats`. An application depending on
the pager crate can call that function and get back a `PagerStats`. Internally,
the pager crate will issue the correct instructions to jump from the security context
of the application that made the call into the security context for the pager-srv crate. This is implemented as the pager-srv crate building a cdylib that can be
loaded into a separate security context, and then linking to that via the pager crate.

The result is that programmers can define "cross-application system calls" for
interprocess communication and more. Calling a secure gate requires the appropriate 
permissions, and not all functions or symbols are secure gates (they must be explicitly defined as such).

## Example

Let's say we have a service we want to provide that exposes a `increment_counter`
function. We'll call the service 'counter'. First we'll create the wrapper library
crate, called `counter`. It'll need to have the secgate library (in src/lib) added
as a dependency.

Then we create the service crate, `counter-srv`, as a library, and modify the
Cargo.toml to include the following:

```
[lib]
crate-type = ["cdylib"]

[dependencies]
secgate = { path = <path to secgate, probably ../../lib/secgate> }
counter = { path = <path to counter wrapper library> }

```

In the counter crate's src/lib.rs, we'll add:
```
#[secgate::gatecall]
pub fn increment_counter() -> Result<u32, secgate::TwzError> {}
```

The attribute macro `gatecall` will fill out the function body automatically.
Additionally, note that all secure gates must return `Result<T, TwzError>`.

In the counter-srv crate's src/lib.rs, we can add:
```
#[secgate::entry(lib = "counter")]
pub fn increment_counter() -> Result<u32, secgate::TwzError> {
  static COUNTER: AtomicU32 = AtomicU32::new(0);
  Ok(COUNTER.fetch_add(1, Ordering::SeqCst))
}
```

The `lib = "counter"` option allows the `entry` macro to type check against the
definition given in `counter`'s src/lib.rs.

## Getting Caller Information

The counter-srv increment_counter implementation can get access to caller
information via the secgate::get_caller() function. This function returns
a `GateCallInfo`, which can be used to get the caller's thread ID and the
security context ID.
