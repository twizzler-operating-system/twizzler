#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use rdrand::RdSeed;

#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
mod rndrs;
#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
use rand_core::RngCore;

#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
use self::rndrs::Rndrs;
use super::{register_entropy_source, EntropySource};

pub struct CpuEntropy {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    cpu: RdSeed,
    #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
    cpu: Rndrs,
}

impl CpuEntropy {}

impl EntropySource for CpuEntropy {
    fn try_new() -> Result<Self, ()> {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        let cpu = RdSeed::new().or(Err(()))?;
        #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
        let cpu = Rndrs::try_new().or(Err(()))?;
        Ok(Self { cpu })
    }

    #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
    fn try_fill_entropy(&mut self, dest: &mut [u8]) -> Result<(), ()> {
        Ok(self.cpu.try_fill_entropy(dest).map_err(|_| ())?)
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    fn try_fill_entropy(&mut self, dest: &mut [u8]) -> Result<(), ()> {
        Ok(self.cpu.try_fill_bytes(dest).map_err(|_| ())?)
    }
}

pub fn maybe_add_cpu_entropy_source() -> bool {
    register_entropy_source::<CpuEntropy>()
}

mod test {
    use twizzler_kernel_macros::kernel_test;

    use super::*;
    #[kernel_test]
    fn test_rand() {
        let generator = CpuEntropy::try_new();
        if let Ok(mut generator) = generator {
            let mut dest: [u8; 8] = [0; 8];
            generator
                .try_fill_entropy(&mut dest)
                .expect("CpuEntropy should return some bytes");
            logln!("Random bytes from CpuEntropy: {:?}\n", dest);
        } else {
            logln!("CpuEntropy not supported on this hardware")
        }
    }
}
