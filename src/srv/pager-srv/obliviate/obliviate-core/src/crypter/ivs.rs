use std::convert::Infallible;

use super::Ivg;

pub struct SequentialIvg {
    counter: Vec<u8>,
}

impl SequentialIvg {
    pub fn new(iv_len: usize) -> Self {
        Self {
            counter: vec![0; iv_len],
        }
    }

    fn inc(&mut self) {
        for byte in self.counter.iter_mut() {
            if *byte == u8::MAX {
                *byte = 0;
            } else {
                *byte += 1;
                break;
            }
        }
    }
}

// TODO: This is kinda bad, but whatever
impl Default for SequentialIvg {
    fn default() -> Self {
        Self::new(16)
    }
}

impl Ivg for SequentialIvg {
    type Error = Infallible;

    fn gen(&mut self, iv: &mut [u8]) -> Result<(), Self::Error> {
        assert!(iv.len() >= self.counter.len(), "incorrect IV length");
        iv.copy_from_slice(&self.counter);
        self.inc();
        Ok(())
    }
}
