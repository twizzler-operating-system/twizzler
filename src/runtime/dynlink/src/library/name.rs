pub struct LibraryName<'a>(&'a [u8]);

impl<'a> From<&'a str> for LibraryName<'a> {
    fn from(value: &'a str) -> Self {
        Self(value.as_bytes())
    }
}
