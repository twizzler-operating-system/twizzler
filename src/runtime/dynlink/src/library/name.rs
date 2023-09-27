pub struct LibraryName<'a>(pub &'a [u8]);

impl<'a> From<&'a str> for LibraryName<'a> {
    fn from(value: &'a str) -> Self {
        Self(value.as_bytes())
    }
}

impl<'a> core::fmt::Debug for LibraryName<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LibraryName({})", String::from_utf8_lossy(self.0))
    }
}
