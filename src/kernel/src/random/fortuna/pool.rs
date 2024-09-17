use core::{cell::RefCell, slice::IterMut};

use digest::Digest;
use sha2::Sha256;

const BUFFER_SIZE: usize = 32;

// based on Cryptography Engineering Chapter 9 by Neils Ferguson et. al.
// comments including 9.x.x reference the above text's sections

// 9.5.2
#[derive(Debug)]
pub struct Pool {
    state: Sha256,
    buf: [u8; BUFFER_SIZE],
    buf_offset: usize,
    count: usize,
}

impl Pool {
    pub fn new() -> Pool {
        let mut buf: [u8; BUFFER_SIZE] = Default::default();
        Pool {
            state: Sha256::new(),
            buf: Default::default(),
            buf_offset: 0,
            count: 0,
        }
    }
    // 9.5.3.2 only update the internal state when the buf is full of entropy
    pub fn insert(&mut self, data: &[u8]) {
        let mut data_iter = data.iter();
        let mut buf_slice = &mut self.buf[self.buf_offset..];
        let mut buf_iter = buf_slice.iter_mut();
        while data_iter.len() != 0 {
            for into in buf_iter {
                if let Some(&value) = data_iter.next() {
                    *into = value;
                    self.buf_offset += 1;
                } else {
                    break;
                }
            }

            if data_iter.len() > 0 {
                self.state.update(data);
                self.buf_offset = 0;
            }
            buf_slice = &mut self.buf[self.buf_offset..];
            buf_iter = buf_slice.iter_mut();
        }
        self.count += data.len();
    }

    pub fn result(&mut self, out: &mut [u8]) {
        self.state.finalize_into_reset(out.into());
        self.state.update(&out);
        self.state.finalize_into_reset(out.into());
        self.count = 0;
    }

    pub fn count(&self) -> usize {
        self.count
    }
}
