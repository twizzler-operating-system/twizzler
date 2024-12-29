//! Transformations of random streams.
//!
//! All these wrappers maps strong pseudorandom streams to strong pseudorandom stream. But if the
//! original stream happens to be weak, the purpose of these transformations is to weaken them
//! further such that the weaknesses can be detected easily.

use super::Random;

/// Skip every other number.
pub struct SkipOne<R>(pub R);

impl<R: Random> Random for SkipOne<R> {
    fn get_random(&mut self) -> u64 {
        self.0.get_random();
        self.0.get_random()
    }
}

/// Skip every second number.
pub struct SkipTwo<R>(pub R);

impl<R: Random> Random for SkipTwo<R> {
    fn get_random(&mut self) -> u64 {
        self.0.get_random();
        self.0.get_random();
        self.0.get_random()
    }
}

/// Concatenate the last 32 bits of two adjacent numbers.
pub struct Concatenate32<R>(pub R);

impl<R: Random> Random for Concatenate32<R> {
    fn get_random(&mut self) -> u64 {
        (self.0.get_random() << 32) | (self.0.get_random() & 0xFFFFFFFF)
    }
}

/// XOR numbers next to each other.
pub struct Xor<R>(pub R);

impl<R: Random> Random for Xor<R> {
    fn get_random(&mut self) -> u64 {
        self.0.get_random() ^ self.0.get_random()
    }
}

/// Add numbers next to each other.
pub struct Add<R>(pub R);

impl<R: Random> Random for Add<R> {
    fn get_random(&mut self) -> u64 {
        self.0.get_random().wrapping_add(self.0.get_random())
    }
}

/// Multiply the number with the next number rounded up to the nearest odd number.
pub struct Multiply<R>(pub R);

impl<R: Random> Random for Multiply<R> {
    fn get_random(&mut self) -> u64 {
        // We OR the random number with 1 to make sure it is odd, and thus relatively prime to the
        // modulo, we work in. This means that the transformation is bijective.
        self.0.get_random().wrapping_mul(self.0.get_random() | 1)
    }
}

/// Collect an integer from the least significant bit.
pub struct LastBit<R>(pub R);

impl<R: Random> Random for LastBit<R> {
    fn get_random(&mut self) -> u64 {
        let mut x = self.0.get_random() & 1;
        for _ in 1..32 {
            x <<= 1;
            x |= self.0.get_random() & 1;
        }

        x
    }
}

/// Modular multiply the number by three.
pub struct MultiplyByThree<R>(pub R);

impl<R: Random> Random for MultiplyByThree<R> {
    fn get_random(&mut self) -> u64 {
        self.0.get_random().wrapping_mul(3)
    }
}

/// Modular divide the number by three (multiply by three's inverse over the ring).
pub struct ModularDivideByThree<R>(pub R);

impl<R: Random> Random for ModularDivideByThree<R> {
    fn get_random(&mut self) -> u64 {
        // 12297829382473034411 is the multiplicative inverse of 3 in the ring Z/2^64Z.
        self.0.get_random().wrapping_mul(12297829382473034411)
    }
}

/// Transform to Hamming weight.
///
/// Since this can maximally take value 64, it has 6 bits of information each. So, we need to
/// extract 11 values to reach 64 bits of information.
pub struct Hamming<R>(pub R);

impl<R: Random> Random for Hamming<R> {
    fn get_random(&mut self) -> u64 {
        self.0.get_random().count_ones() as u64
            | (self.0.get_random().count_ones() as u64) << 6
            | (self.0.get_random().count_ones() as u64) << 12
            | (self.0.get_random().count_ones() as u64) << 18
            | (self.0.get_random().count_ones() as u64) << 24
            | (self.0.get_random().count_ones() as u64) << 30
            | (self.0.get_random().count_ones() as u64) << 36
            | (self.0.get_random().count_ones() as u64) << 42
            | (self.0.get_random().count_ones() as u64) << 48
            | (self.0.get_random().count_ones() as u64) << 54
            | (self.0.get_random().count_ones() as u64) << 60
    }
}

/// Skip the next number if the current number is even.
pub struct ParitySkip<R>(pub R);

impl<R: Random> Random for ParitySkip<R> {
    fn get_random(&mut self) -> u64 {
        let r = self.0.get_random();

        if r & 1 == 0 {
            self.0.get_random();
        }

        r
    }
}

/// Rotate to the left by 7 bits.
pub struct Rol7<R>(pub R);

impl<R: Random> Random for Rol7<R> {
    fn get_random(&mut self) -> u64 {
        self.0.get_random().rotate_left(7)
    }
}
