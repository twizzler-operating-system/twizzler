pub fn rand_32() -> [u8; 32] {
    let mut dest = [0 as u8; 32];
    // ideally we dont unwrap here?
    getrandom::getrandom(&mut dest).unwrap();
    dest
}
