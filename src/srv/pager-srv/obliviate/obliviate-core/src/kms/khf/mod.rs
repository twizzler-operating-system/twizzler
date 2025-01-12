mod error;
mod khf;
// mod kht;
// mod lethe;

pub(self) mod node;
pub(self) mod topology;
mod khf_tree;

pub use {
    error::{Error, Result},
    khf::{Khf, KhfBuilder, KhfStats},
    // lethe::{Lethe, LetheBuilder, LetheStats},
};

pub(self) type Pos = (u64, u64);
