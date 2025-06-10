use std::{
    ffi::c_void,
    ptr::{addr_of_mut, null_mut},
};

use ext4::{ext4_blockdev, ext4_device_register, ext4_mount};

mod ext4;
//mod ext4_fs;

unsafe extern "C" fn bd_open(bd: *mut ext4_blockdev) -> i32 {
    todo!()
}

unsafe extern "C" fn bd_close(bd: *mut ext4_blockdev) -> i32 {
    todo!()
}

unsafe extern "C" fn bd_bread(
    bd: *mut ext4_blockdev,
    buf: *mut c_void,
    block: u64,
    bcount: u32,
) -> i32 {
    todo!()
}

unsafe extern "C" fn bd_bwrite(
    bd: *mut ext4_blockdev,
    buf: *const c_void,
    block: u64,
    bcount: u32,
) -> i32 {
    todo!()
}

fn main() {
    println!("Hello, world, from Rust!");
    let mut buf = [0u8; 512];
    let mut iface = ext4::ext4_blockdev_iface {
        open: Some(bd_open),
        bread: Some(bd_bread),
        bwrite: Some(bd_bwrite),
        close: Some(bd_close),
        lock: None,
        unlock: None,
        ph_bsize: 512,
        ph_bcnt: 1000000,
        ph_bbuf: buf.as_mut_ptr(),
        ph_refctr: 0,
        bread_ctr: 0,
        bwrite_ctr: 0,
        p_user: null_mut(),
    };
    let mut bd = ext4::ext4_blockdev {
        bdif: addr_of_mut!(iface),
        part_offset: 0,
        part_size: 0,
        bc: null_mut(),
        lg_bsize: 4096,
        lg_bcnt: 1000000,
        cache_write_back: 0,
        fs: null_mut(),
        journal: null_mut(),
    };
    let r = unsafe { ext4_device_register(&mut bd, c"store".as_ptr()) };
    println!("==> {}", r);
    let r = unsafe { ext4_mount(c"store".as_ptr(), c"mnt/".as_ptr(), false) };
    println!("==> {}", r);
    unsafe { c_hello_world() };
}

#[link(name = "hw")]
unsafe extern "C" {
    fn c_hello_world() -> std::ffi::c_int;
}
