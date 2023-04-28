use hasher::Hasher;

pub struct BadHasher<const N: usize> {
    digest: [u8; N],
}

impl<const N: usize> Hasher<N> for BadHasher<N> {
    fn new() -> Self {
        Self { digest: [0; N] }
    }

    fn update(&mut self, data: &[u8]) {
        self.digest.iter_mut().zip(data.iter()).for_each(|(x, y)| {
            *x = *y;
        });
    }

    fn finish(self) -> [u8; N] {
        self.digest
    }

    fn digest(data: &[u8]) -> [u8; N] {
        let mut hasher = Self::new();
        hasher.update(data);
        hasher.finish()
    }
}
