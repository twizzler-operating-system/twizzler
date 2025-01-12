use paste::paste;
use sha3::Digest;

use super::Hasher;

macro_rules! hasher_impl {
    ($hasher:ident, $size:literal) => {
        paste! {
            use sha3::$hasher as [<Inner $hasher>];

            pub struct $hasher([<Inner $hasher>]);

            pub const [<$hasher:upper _MD_SIZE>]: usize = $size;

            impl Hasher<$size> for $hasher {
                fn new() -> Self {
                    Self([<Inner $hasher>]::new())
                }

                fn update(&mut self, data: &[u8]) {
                    Digest::update(&mut self.0, data);
                }

                fn finish(self) -> [u8; [<$hasher:upper _MD_SIZE>]] {
                    Digest::finalize(self.0).into()
                }

                fn digest(data: &[u8]) -> [u8; [<$hasher:upper _MD_SIZE>]] {
                    let mut hasher = Self::new();
                    hasher.update(data);
                    hasher.finish()
                }
            }
        }
    };
}

hasher_impl!(Sha3_224, 28);
hasher_impl!(Sha3_256, 32);
hasher_impl!(Sha3_384, 48);
hasher_impl!(Sha3_512, 64);

#[cfg(test)]
mod tests {

    use super::*;

    macro_rules! hasher_test_impl {
        ($hasher:ident, $expected:literal) => {
            paste! {
                #[test]
                fn [<$hasher:lower>]() {
                    assert_eq!(
                        hex::encode($hasher::digest(b"abcd")),
                        $expected
                    )
                }
            }
        };
    }

    hasher_test_impl!(
        Sha3_224,
        "dd886b5fd8421fb3871d24e39e53967ce4fc80dd348bedbea0109c0e"
    );

    hasher_test_impl!(
        Sha3_256,
        "6f6f129471590d2c91804c812b5750cd44cbdfb7238541c451e1ea2bc0193177"
    );

    hasher_test_impl!(
        Sha3_384,
        "5af1d89732d4d10cc6e92a36756f68ecfbf7ae4d14ed4523f68fc304cccfa5b0bba01c80d0d9b67f9163a5c211cfd65b"
    );

    hasher_test_impl!(
        Sha3_512,
        "6eb7b86765bf96a8467b72401231539cbb830f6c64120954c4567272f613f1364d6a80084234fa3400d306b9f5e10c341bbdc5894d9b484a8c7deea9cbe4e265"
    );
}
