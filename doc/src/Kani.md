# A Comprehensive Guide to Using Kani with Twizzler

This guide provides a step-by-step walkthrough for using Kani within Twizzler. Kani is a Rust verification tool that uses model checking to verify the correctness of Rust programs. You’ll learn how to install kani, and create and run harnesses.

---

## 1. Install Twizzler

Install Twizzler by following the [build guide](https://twizzler-operating-system.github.io/nightly/book/BUILD.html) up to the 
```bash
cargo bootstrap
```
stage.

---

## 2. Install Kani Locally

Before using Kani with Twizzler, you need to install Kani and ensure that it’s properly set up. Kani uses CBMC (C Bounded Model Checker) as its backend for formal verification, so both need to be installed.

### Steps:
1. **Install Kani:**
    You should check to see if Kani is already installed using
    ```bash
    cargo kani --version
    ```
and
    ```bash
    cmbc --version
    ```
If the version of Kani is below 0.56.0 or the CBMC version is below 6.1.1, or if neither are installed, use the following command to install:
	```bash
	cargo install --locked kani-verifier
	cargo kani setup
	```

If you need the full install guide, refer to the [Kani website](https://model-checking.github.io/kani/install-guide.html).

3. **Check Kani Version:**
   Verify that Kani is correctly installed and the version is above 0.56.0:

   ```bash
   cargo kani --version
   ```

5. **Check CBMC Version:**
   Ensure that the CBMC version is above 6.1.1:

   ```bash
   cbmc --version
   ```
---

## 3. Running the Existing Harnesses and Checking the Output

Twizzler’s codebase already has some pre-existing Kani harnesses. You can run these harnesses to ensure that everything is functioning correctly.

### Steps:
2. **Run the Kani Harnesses:**
   From the root directory, run
   ```bash
   cargo kani_twiz
   ```
   You can also run kani without our wrapper for greater flexibility but read section 6 on how to get it working. 
   If you want to run a specific harness, specify the path:
   ```bash
cargo kani --enable-unstable --ignore-global-asm -Zstubbing --workspace --exclude monitor unicode-bidi --harness example_harness
   ```

3. **Check the Output:**
   Kani will check the correctness of the code based on the verification properties in the harness. You should see a report indicating whether the properties hold or if any assertion failures or errors occurred.

---

## 4. Creating Harnesses

Harnesses are crucial for formally verifying parts of your Rust code. In this section, you’ll learn how to write a simple Kani harness for Twizzler and explore key Kani capabilities.

### Writing a Basic Kani Harness:
A Kani harness is a function that defines specific verification conditions. Here’s a simple example:

```rust
#[kani::proof]
fn example_harness() {
    let x: u32 = kani::any();  // Generate a non-deterministic value for x
    let y = x + 1;
    assert!(y > x);            // Check if the assertion holds
}
```

In this harness:
- `kani::any()` generates a non-deterministic value for `x`, which allows Kani to check all possible values.
- The `assert!()` macro checks that `y` is greater than `x` for all possible values of `x`.

### Testing the Harness:
1. **Run the Harness:**
   Run the above harness using:
   ```bash
   cargo kani --enable-unstable --ignore-global-asm -Zstubbing --workspace --exclude monitor unicode-bidi --harness example_harness
   ```

2. **Analyze the Output:**
   Kani should output something similar to the following 

```bash
RESULTS:
Check 1: example::example_harness.assertion.1
	 - Status: FAILURE
	 - Description: "attempt to add with overflow"
	 - Location: example.rs:211:17 in function example::example_harness

Check 2: example::example_harness.assertion.2
	 - Status: SUCCESS
	 - Description: "assertion failed: y > x"
	 - Location: example.rs:212:9 in function example::example_harness


SUMMARY:
 ** 1 of 2 failed
Failed Checks: attempt to add with overflow

VERIFICATION:- FAILED
```

---

## 5. GitHub Actions 

Github Actions are provided. It will run all harnesses in the repository. Simply make a pull request against main and Kani will run to ensure harnesses pass.

---

## 6. Important Notes
1. **NVME Controller Incorrect Size Error**
When running Kani, you may run into the following error:
```bash

error[E0308]: mismatched types
   --> src/lib/nvme-rs/src/ds/identify/controller.rs:134:37
    |
134 | const _SIZE_CHECKER: [u8; 0x1000] = [0; std::mem::size_of::<IdentifyControllerDataStructure>()];
    |                           ------    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected an array with a fixed size of 4096 elements, found one with 4112 elements
    |                           |
    |                           help: consider specifying the actual array length: `4112`

``` 
As cargo suggests, the issues lies in the array length. It seems to be related to kani\_metadata, but to fix it there are two scripts run during 
```bash
cargo kani_twiz
```
The script 
```bash
admin_scripts/kani_nvme_controller_value.sh
```
will change the value to 4112 and
```bash
admin_scripts/twizzler_nvme_controller_value.sh
```
back to 0x1000. If the script is stopped in the middle of execution, this change will not be correctly fixed and 
```bash
cargo build-all
```
will fail. To fix it, you can manually run the script
```bash
admin_scripts/twizzler_nvme_controller_value.sh
```

2. Cargo kani 
When using 
```bash
cargo kani
```
(ie without kani\_twiz), then you need the following command arguments to work properly:
```bash
cargo kani --enable-unstable --ignore-global-asm -Zstubbing --workspace --exclude monitor unicode-bidi
```

If selecting only one harness:
```bash
cargo kani --enable-unstable --ignore-global-asm -Zstubbing --workspace --exclude monitor unicode-bidi --harness example_harness
```

---

## 7. Questions

### Frequently Asked Questions:

**Q1. What are the advantages of using Kani with Twizzler?**
   - Kani allows you to formally verify the correctness of your Rust code, ensuring memory safety, integer safety, and adherence to assertions. When integrated with Twizzler, you can use Kani to ensure that key components of the OS are verified for safety and correctness.

**Q2. How can I debug a failing Kani harness?**
   - Kani provides counterexamples for failing harnesses, which show the input values that led to the failure. You can use this information to debug the code and refine your harness.

**Q3. Does Kani support concurrency verification?**
   - As of now, Kani only supports sequential verification.

---
