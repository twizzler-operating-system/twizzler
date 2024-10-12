## Kani on the Twizzler ABI

We can verify the ABI at least!

*Most useful reference:*
https://model-checking.github.io/kani/

*Verification:*
grep for KANI_TODO to find verfication targets.

## Install
Kani is supported as an external tool on cargo so we quickly install it!

```
cargo install --locked kani-verifier
cargo kani setup
```
## Commands
Once the install is done you can verify the entire crate by simply running in the crates root directory
```
cargo kani
```

You can also varify a single file by doing the following, altough this is limited by dependencies external to the file
```
kani <file_name> 
```

### More Commands
Get a trace for a verification failure
```
cargo kani  --enable-unstable --visualize
```
Get a print of a test that will trigger the failure
```
cargo kani --enable-unstable  --concrete-playback=print|inplace
```
Run a single test harness
```
cargo kani --harness <harness_name>
```
Override unwind value for bound checking
```
cargo kani --harness <harness_name> --unwind <value>
```

## Test Harness

Basic proof harness
```
#[cfg(kani)]
#[kani::proof]
fn test_function(){
    // test harness
    kani::any()
    kani::assume()
    // etc...
}
```

Proof harness with 
```
#[cfg(kani)]
#[kani::proof]
#[kani::unwind(1)] // Limit all loops executed by harness to 1 iteration
fn test_function(){
    // test harness
    kani::any()
    kani::assume()
    // etc...
}
```





