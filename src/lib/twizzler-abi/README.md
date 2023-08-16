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
Get a print of a text that will trigger the failure
```
cargo kani --enable-unstable  --concrete-playback=print|inplace
```

## Test Harness
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









