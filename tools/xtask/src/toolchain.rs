pub struct Toolchain {
    name: String,
}

impl Toolchain {
    pub fn name(&self) -> &String {
        &self.name
    }
}
