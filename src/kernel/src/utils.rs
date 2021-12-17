
pub fn align<T: From<usize> + Into<usize>>(val: T, align: usize) -> T {
    let val = val.into();
    if val == 0 {
        return val.into();
    }
    let res: usize = ((val - 1) & !(align - 1)) + align;
    res.into()
}
