#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use rdrand::{ErrorCode, RdSeed};

#[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
mod rndrs;

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
        let cpu = Rndrs::new().or(())?;
        Ok(Self { cpu })
    }
    fn try_fill_entropy(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        Ok(self.cpu.try_fill_bytes(dest)?)
    }
}

pub fn maybe_add_cpu_entropy_source() {
    if let Ok(cpu_entropy) = CpuEntropy::try_new() {
        register_entropy_source::<CpuEntropy>()
    }
}

mod test {
    use twizzler_kernel_macros::kernel_test;

    use super::*;
    #[kernel_test]
    fn test_rand() {
        let mut generator = CpuEntropy::try_new();
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
