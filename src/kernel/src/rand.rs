use core::arch::asm;

use rand_chacha::ChaCha20Rng;
use rdrand::RdSeed;

#[cfg(target_arch = "x86_64")]
pub fn get_random_seed() -> Option<u64> {
    let s = RdSeed::new().unwrap();
    s.try_next_u64().ok()
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
