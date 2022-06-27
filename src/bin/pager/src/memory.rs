pub struct Page {
    phys: u64,
    virt: u64,
}

struct Mapper {
    free: Vec<Page>,
}
