use rand::{CryptoRng, RngCore};

pub struct BadRng {
    state: u32,
}

impl BadRng {
    pub fn new() -> Self {
        Self { state: 0 }
    }

    // Courtesy of the pager crate.
    fn cycle(&mut self) -> u32 {
        self.state = self.state.wrapping_mul(69069).wrapping_add(5);
        self.state >> 16
    }
}

impl RngCore for BadRng {
    fn next_u32(&mut self) -> u32 {
        self.cycle()
    }

    fn next_u64(&mut self) -> u64 {
        self.cycle() as u64
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        dest.iter_mut()
            .for_each(|byte| *byte = (self.cycle() & 0xff) as u8);
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand::Error> {
        Ok(self.fill_bytes(dest))
    }
}

// Truly a security crime.
impl CryptoRng for BadRng {}
