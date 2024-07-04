use core::arch::asm;

use rand_chacha::ChaCha20Rng;
use rdrand::RdSeed;

#[cfg(target_feature = "rdrand")]
pub fn get_random_seed() -> Option<u64> {
    let s = RdSeed::new().unwrap();
    s.try_next_u64().ok()
}

#[cfg(target_arch = "aarch64")]
pub fn get_random_seed_arch() -> Option<u64> {
    // https://github.com/CTSRD-CHERI/cheribsd/blob/bdeff30fb6b1744816f43ed8a3c2f0a133d872c1/sys/dev/random/armv8rng.c#L54-L73
    todo!();
}

mod test {
    use twizzler_kernel_macros::kernel_test;

    use super::*;
    #[kernel_test]
    fn test_rand() {
        let _seed: u64 = get_random_seed().unwrap();
        // println!("{seed}");
    }
}
