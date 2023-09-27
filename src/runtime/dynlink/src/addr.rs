pub struct Address(usize);

impl From<usize> for Address {
    fn from(value: usize) -> Self {
        Self(value)
    }
}

impl<T> From<*const T> for Address {
    fn from(value: *const T) -> Self {
        Self(value.addr())
    }
}
