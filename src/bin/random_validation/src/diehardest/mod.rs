pub mod analysis;
pub mod transform;

/// A random number generator.
pub trait Random {
    /// Get a random number.
    fn get_random(&mut self) -> u64;
}

/// 13 tests in total implemented
pub const _TOTAL_TESTS: u32 = 13;
pub const _EXPECTED_SCORE: u32 = _TOTAL_TESTS * 1024;
pub fn _crush<R: Random + Clone>(rand: R) -> u32 {
    analysis::Report::new(rand.clone()).get_score().total() as u32
        + analysis::Report::new(transform::SkipOne(rand.clone()))
            .get_score()
            .total() as u32
        + analysis::Report::new(transform::SkipTwo(rand.clone()))
            .get_score()
            .total() as u32
        + analysis::Report::new(transform::Concatenate32(rand.clone()))
            .get_score()
            .total() as u32
        + analysis::Report::new(transform::Xor(rand.clone()))
            .get_score()
            .total() as u32
        + analysis::Report::new(transform::Add(rand.clone()))
            .get_score()
            .total() as u32
        + analysis::Report::new(transform::Multiply(rand.clone()))
            .get_score()
            .total() as u32
        + analysis::Report::new(transform::LastBit(rand.clone()))
            .get_score()
            .total() as u32
        + analysis::Report::new(transform::MultiplyByThree(rand.clone()))
            .get_score()
            .total() as u32
        + analysis::Report::new(transform::ModularDivideByThree(rand.clone()))
            .get_score()
            .total() as u32
        + analysis::Report::new(transform::Hamming(rand.clone()))
            .get_score()
            .total() as u32
        + analysis::Report::new(transform::ParitySkip(rand.clone()))
            .get_score()
            .total() as u32
        + analysis::Report::new(transform::Rol7(rand.clone()))
            .get_score()
            .total() as u32
}
