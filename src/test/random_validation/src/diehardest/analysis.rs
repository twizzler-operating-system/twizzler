//! Analysis of pseudorandom streams.

use std::{cmp, collections::HashSet};

use super::Random;

const DISTRIBUTION_MODULO: u32 = 4096;
const DEPENDENCY_MATRIX_SIZE: usize = 64;
const LOOP_COUNT: u32 = 1 << 16;

/// A analysis report extracted from some stream.
pub struct Report {
    /// The index in which the first number is returned again.
    ///
    /// If it is never found again, the value is 0.
    cycle_length: u32,
    /// The number of colliding numbers found in the sample of the stream.
    collisions: u8,
    /// The bit dependency matrix.
    ///
    /// This contains the probability that bit `x` is set if bit `y` is, i.e. `p(y|x)`.
    dependency_matrix: [[u32; DEPENDENCY_MATRIX_SIZE]; DEPENDENCY_MATRIX_SIZE],
    /// The distribution of the sample, modulo 4096.
    distribution: [u16; DISTRIBUTION_MODULO as usize],
}

impl Default for Report {
    fn default() -> Report {
        Report {
            cycle_length: 0,
            collisions: 0,
            dependency_matrix: [[0; DEPENDENCY_MATRIX_SIZE]; DEPENDENCY_MATRIX_SIZE],
            distribution: [0; DISTRIBUTION_MODULO as usize],
        }
    }
}

impl Report {
    /// Investigate a random stream and create a report.
    pub fn new<R: Random>(mut rand: R) -> Report {
        let mut report = Report::default();
        let mut set = HashSet::new();

        let start = rand.get_random();
        for n in 0..LOOP_COUNT {
            // Collect a random number.
            let r = rand.get_random();

            // Update the bit depedency matrix.
            for x in 0..DEPENDENCY_MATRIX_SIZE {
                for y in 0..DEPENDENCY_MATRIX_SIZE {
                    report.dependency_matrix[x][y] +=
                        ((r & (1 << x) == 0) <= (r & (1 << y) == 0)) as u32;
                }
            }

            // Increment the distribution entry.
            report.distribution[r as usize % DISTRIBUTION_MODULO as usize] += 1;

            // If it returned to the first number, set the cycle length.
            if report.cycle_length == 0 && r == start {
                report.cycle_length = n;
            }

            // Insert the random number into the set and update the collision number.
            report.collisions += (!set.insert(r)) as u8;
        }

        report
    }

    /// Get the final score of this report.
    pub fn get_score(&self) -> Score {
        Score {
            // The cycle should not be less than the sample size.
            cycle: if self.cycle_length == 0 { 255 } else { 0 },
            // Ideally, there should be no collisions in our sample. Applying the birthday problem
            // still gives us very small probability of such a collision occuring.
            collision: match self.collisions {
                0 => 255,
                1 => 20,
                _ => 0,
            },
            bit_dependency: {
                // Calculate the minimum and maximum entry of the dependency matrix.
                let mut max = 0;
                let mut min = !0;
                for x in 0..64 {
                    for y in 0..64 {
                        max = cmp::max(self.dependency_matrix[x][y], max);
                        min = cmp::min(self.dependency_matrix[x][y], min);
                    }
                }

                println!("bit_dep min: {}, max: {}", min, max);

                const IDEAL_MIN: i32 = 0;
                const IDEAL_MAX: i32 = LOOP_COUNT as i32;

                // Rate it based on it's distance to the ideal value.
                let pmin = match (min as i32 - IDEAL_MIN).abs() {
                    0..=3 => 127,
                    4..=5 => 126,
                    6..=15 => 120,
                    16..=31 => 90,
                    32..=63 => 50,
                    64..=79 => 20,
                    _ => 0,
                };

                // Rate it based on it's distance to the ideal value.
                let pmax = match (max as i32 - IDEAL_MAX).abs() {
                    0..=3 => 127,
                    4..=5 => 126,
                    6..=15 => 120,
                    16..=31 => 90,
                    32..=63 => 50,
                    64..=79 => 20,
                    _ => 0,
                };

                pmin + pmax + 1
            },
            distribution: {
                // Calculate the minimum and maximum entry of the distribution array.
                let mut max = 0;
                let mut min = !0;
                for i in 0..4096 {
                    max = cmp::max(self.distribution[i], max);
                    min = cmp::min(self.distribution[i], min);
                }

                println!("dist min: {}, max: {}", min, max);

                const IDEAL_BUCKET_CT: i32 = (LOOP_COUNT / DISTRIBUTION_MODULO) as i32;

                // Rate it based on it's distance to the ideal value.
                let pmin = match (IDEAL_BUCKET_CT - min as i32).abs() {
                    0..=3 => 127,
                    4..=5 => 126,
                    6..=9 => 110,
                    10..=14 => 70,
                    15..=17 => 50,
                    18..=19 => 30,
                    20..=32 => 20,
                    _ => 0,
                };

                // Rate it based on it's distance to the ideal value.
                let pmax = match (max as i32 - IDEAL_BUCKET_CT).abs() {
                    0..=3 => 127,
                    4..=5 => 126,
                    6..=9 => 110,
                    10..=14 => 70,
                    15..=17 => 50,
                    18..=19 => 30,
                    20..=32 => 20,
                    _ => 0,
                };

                pmin + pmax + 1
            },
        }
    }
}

/// The score of some report.
#[derive(Debug)]
pub struct Score {
    /// The quality of the cycle length.
    cycle: u8,
    /// The quality of occurence of collisions.
    collision: u8,
    /// The quality of the BIC.
    bit_dependency: u8,
    /// The quality of the distribution.
    distribution: u8,
}

impl Score {
    /// Sum the scores together to a single integer.
    pub fn total(self) -> u16 {
        self.cycle as u16
            + self.collision as u16
            + self.bit_dependency as u16
            + self.distribution as u16
    }
}
