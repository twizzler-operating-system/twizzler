use std::{iter::Peekable, marker::PhantomData};

use lru_mem::HeapSize;
use serde::{Deserialize, Serialize};

use crate::{hasher::Hasher, key::Key};

use super::Pos;

#[derive(Deserialize, Serialize, Clone, HeapSize, Debug)]
pub struct Topology {
    descendants: Vec<u64>,
}

impl Default for Topology {
    fn default() -> Self {
        Self::new(&[4, 4, 4, 4])
    }
}

impl Topology {
    pub fn new(fanouts: &[u64]) -> Self {
        let mut leaves = fanouts.iter().product();
        let mut descendants = Vec::with_capacity(fanouts.len() + 2);

        descendants.push(0);
        for fanout in fanouts {
            descendants.push(leaves);
            leaves /= fanout;
        }
        descendants.push(1);

        Self { descendants }
    }

    pub fn height(&self) -> u64 {
        self.descendants.len() as u64
    }

    pub fn fanout(&self, level: u64) -> u64 {
        if level == 0 {
            0
        } else if level == self.height() as u64 {
            1
        } else {
            self.descendants[level as usize] / self.descendants[(level as usize) + 1]
        }
    }

    pub fn descendants(&self, level: u64) -> u64 {
        self.descendants[level as usize]
    }

    pub fn start(&self, node: Pos) -> u64 {
        if node.0 == 0 {
            0
        } else {
            node.1 * self.descendants[node.0 as usize]
        }
    }

    pub fn end(&self, node: Pos) -> u64 {
        if node.0 == 0 {
            0
        } else {
            self.start(node) + self.descendants[node.0 as usize]
        }
    }

    pub fn range(&self, node: Pos) -> Pos {
        (self.start(node), self.end(node))
    }

    pub fn offset(&self, leaf: u64, level: u64) -> u64 {
        if level == 0 {
            0
        } else {
            leaf / self.descendants[level as usize]
        }
    }

    pub fn is_ancestor(&self, n: Pos, m: Pos) -> bool {
        let (n_start, n_end) = self.range(n);
        let (m_start, m_end) = self.range(m);
        m != (0, 0) && (n == (0, 0) || (n_start <= m_start && m_end <= n_end))
    }

    pub fn is_parent(&self, n: Pos, m: Pos) -> bool {
        n.0 + 1 == m.0 && self.is_ancestor(n, m)
    }

    pub fn leaf_position(&self, leaf: u64) -> Pos {
        (self.height() - 1, leaf)
    }

    pub fn path(&self, from: Pos, to: Pos) -> Path<'_> {
        Path::new(self, from, to)
    }

    pub fn parent_path(&self, from: Pos, to: Pos) -> ParentPath<'_> {
        ParentPath::new(self.path(from, to))
    }

    pub fn derivation_path<H, const N: usize>(
        &self,
        start_key: Key<N>,
        from: Pos,
        to: Pos,
    ) -> DerivationPath<Path, H, N> {
        DerivationPath::new(start_key, self.path(from, to))
    }

    pub fn parent_derivation_path<H, const N: usize>(
        &self,
        start_key: Key<N>,
        from: Pos,
        to: Pos,
    ) -> DerivationPath<ParentPath, H, N> {
        DerivationPath::new(start_key, self.parent_path(from, to))
    }

    pub fn coverage(&self, level: u64, start: u64, end: u64) -> Coverage<'_> {
        Coverage::new(self, level, start, end)
    }
}

pub struct Path<'a> {
    topology: &'a Topology,
    from: Pos,
    to: Pos,
    done_next: bool,
    done_next_back: bool,
}

impl<'a> Path<'a> {
    pub fn new(topology: &'a Topology, from: Pos, to: Pos) -> Self {
        Self {
            topology,
            from,
            to,
            done_next: false,
            done_next_back: false,
        }
    }
}

impl Iterator for Path<'_> {
    type Item = Pos;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done_next {
            return None;
        }

        let res = self.from;
        if res == self.to {
            self.done_next = true;
        } else {
            let leaf = self.topology.start(self.to);
            let level = self.from.0 + 1;
            let offset = self.topology.offset(leaf, level);
            self.from = (level, offset);
        }

        Some(res)
    }
}

impl DoubleEndedIterator for Path<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.done_next_back {
            return None;
        }

        let res = self.to;
        if res == self.from {
            self.done_next_back = true;
        } else {
            let leaf = self.topology.start(self.to);
            let level = self.to.0 - 1;
            let offset = self.topology.offset(leaf, level);
            self.to = (level, offset);
        }

        Some(res)
    }
}

pub struct ParentPath<'a> {
    inner: Peekable<Path<'a>>,
}

impl<'a> ParentPath<'a> {
    pub fn new(inner: Path<'a>) -> Self {
        Self {
            inner: inner.peekable(),
        }
    }
}

impl Iterator for ParentPath<'_> {
    type Item = Pos;

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.inner.next();
        match self.inner.peek() {
            Some(_) => Some(item.unwrap()),
            None => None,
        }
    }
}

pub struct DerivationPath<P, H, const N: usize> {
    key: Key<N>,
    inner: P,
    on_start: bool,
    _pd: PhantomData<H>,
}

impl<P, H, const N: usize> DerivationPath<P, H, N> {
    pub fn new(key: Key<N>, inner: P) -> Self {
        Self {
            key,
            inner,
            on_start: true,
            _pd: PhantomData,
        }
    }
}

impl<P, H, const N: usize> Iterator for DerivationPath<P, H, N>
where
    P: Iterator<Item = Pos>,
    H: Hasher<N>,
{
    type Item = (Pos, Key<N>);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|pos| {
            if self.on_start {
                self.on_start = false;
                (pos, self.key)
            } else {
                let mut hasher = H::new();
                hasher.update(&self.key);
                hasher.update(&pos.0.to_le_bytes());
                hasher.update(&pos.1.to_le_bytes());
                self.key = hasher.finish();
                (pos, self.key)
            }
        })
    }
}

pub struct Coverage<'a> {
    level: u64,
    start: u64,
    end: u64,
    state: State,
    topology: &'a Topology,
}

enum State {
    Pre(u64),
    Intra,
    Post(u64),
}

impl<'a> Coverage<'a> {
    pub fn new(topology: &'a Topology, level: u64, start: u64, end: u64) -> Self {
        // Easiest way to enforce correctness (for now).
        assert!(0 < level && level < topology.height());
        Self {
            level,
            start,
            end,
            state: State::Pre(topology.height() - 1),
            topology,
        }
    }
}

impl<'a> Iterator for Coverage<'a> {
    type Item = Pos;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start > self.end {
            return None;
        }

        loop {
            match self.state {
                State::Pre(level) => {
                    if level <= self.level {
                        self.state = State::Intra;
                    } else if self.start % self.topology.descendants(level - 1) != 0
                        && self.start + self.topology.descendants(level) <= self.end
                    {
                        let pos = (level, self.topology.offset(self.start, level));
                        self.start += self.topology.descendants(level);
                        return Some(pos);
                    } else {
                        self.state = State::Pre(level - 1);
                    }
                }
                State::Intra => {
                    if self.start + self.topology.descendants(self.level) <= self.end {
                        let pos = (self.level, self.topology.offset(self.start, self.level));
                        self.start += self.topology.descendants(self.level);
                        return Some(pos);
                    } else {
                        self.state = State::Post(self.level + 1);
                    }
                }
                State::Post(level) => {
                    if level >= self.topology.height() {
                        return None;
                    } else if self.start + self.topology.descendants(level) <= self.end {
                        let pos = (level, self.topology.offset(self.start, level));
                        self.start += self.topology.descendants(level);
                        return Some(pos);
                    } else {
                        self.state = State::Post(level + 1);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Topology;

    //                   _______________ (0, 0)
    //                  /
    //          ____ (1, 0) ___
    //         /                \
    //      (2, 0)            (2, 1)
    //     /      \          /      \
    //  (3, 0)   (3, 1)   (3, 2)   (3, 3)
    #[test]
    fn path_iteration() {
        let topology = Topology::new(&[2, 2]);

        let mut path: Vec<_> = topology.path((0, 0), (3, 2)).collect();

        path.reverse();

        let path_rev: Vec<_> = topology.path((0, 0), (3, 2)).rev().collect();

        assert_eq!(path, path_rev);
    }
}
