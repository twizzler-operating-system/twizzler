//! To display the KHF tree for debugging/showing off purposes.
// TODO
use std::collections::HashMap;

use super::{node::Node, Pos};

struct KhfTree<H, const N: usize> {
    levels: Vec<HashMap<Pos, Node<H, N>>>,
}
