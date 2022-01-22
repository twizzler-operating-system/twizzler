#[repr(C)]
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum AuxEntry {
    Null,
    ProgramHeaders(u64, usize),
    Environment(u64),
    Arguments(u64),
}
