use std::{
    ffi::{c_void, CString},
    io::{Read, Result, Seek, Write},
    ptr::null_mut,
    u64,
};

use lwext4::{
    ext4_blockdev, ext4_blockdev_iface, ext4_device_register, ext4_file, ext4_mount, SEEK_CUR,
    SEEK_END, SEEK_SET,
};

mod lwext4;

fn errno_to_result(errno: i32) -> Result<()> {
    match errno {
        0 => Ok(()),
        _ => Err(std::io::Error::from_raw_os_error(errno)),
    }
}

fn result_to_errno(result: Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(err) => err.raw_os_error().unwrap_or_default(),
    }
}

fn result_u32_to_errno(result: Result<u32>) -> i32 {
    match result {
        Ok(_) => 0,
        Err(err) => err.raw_os_error().unwrap_or_default(),
    }
}

unsafe fn get_iface<'a>(bd: *mut ext4_blockdev) -> &'a mut dyn Ext4BlockdevIface {
    let iface = (*(*bd).bdif).p_user.cast::<BdIface>().as_mut().unwrap();
    &mut *iface.iface
}

unsafe extern "C" fn bd_open(bd: *mut ext4_blockdev) -> i32 {
    let iface = get_iface(bd);
    result_to_errno(iface.open())
}

unsafe extern "C" fn bd_close(bd: *mut ext4_blockdev) -> i32 {
    let iface = get_iface(bd);
    result_to_errno(iface.close())
}

unsafe extern "C" fn bd_bread(
    bd: *mut ext4_blockdev,
    buf: *mut c_void,
    block: u64,
    bcount: u32,
) -> i32 {
    let iface = get_iface(bd);
    result_u32_to_errno(iface.read(buf.cast(), block, bcount))
}

unsafe extern "C" fn bd_bwrite(
    bd: *mut ext4_blockdev,
    buf: *const c_void,
    block: u64,
    bcount: u32,
) -> i32 {
    let iface = get_iface(bd);
    result_u32_to_errno(iface.write(buf.cast(), block, bcount))
}

pub trait Ext4BlockdevIface {
    fn phys_block_size(&mut self) -> u32;
    fn phys_block_count(&mut self) -> u64;
    fn open(&mut self) -> Result<()>;
    fn close(&mut self) -> Result<()>;
    fn read(&mut self, buf: *mut u8, block: u64, bcount: u32) -> Result<u32>;
    fn write(&mut self, buf: *const u8, block: u64, bcount: u32) -> Result<u32>;
    fn lock(&mut self) -> Result<()> {
        Ok(())
    }
    fn unlock(&mut self) -> Result<()> {
        Ok(())
    }
}

struct BdIface {
    iface: Box<dyn Ext4BlockdevIface>,
}

pub struct Ext4Blockdev {
    iface: Box<BdIface>,
    raw: Box<ext4_blockdev>,
    raw_iface: Box<ext4_blockdev_iface>,
    buf: Box<[u8]>,
    name: CString,
}

unsafe impl Send for Ext4Blockdev {}
unsafe impl Sync for Ext4Blockdev {}

impl Drop for Ext4Blockdev {
    fn drop(&mut self) {
        unsafe { lwext4::ext4_device_unregister(self.name.as_ptr()) };
    }
}

impl Ext4Blockdev {
    pub fn new(
        iface: impl Ext4BlockdevIface + 'static,
        bsize: u32,
        bcount: u64,
        name: &str,
    ) -> Result<Self> {
        let mut iface = Box::new(iface);
        let mut buf = vec![0u8; iface.phys_block_size() as usize].into_boxed_slice();
        let psz = iface.phys_block_size();
        let pcount = iface.phys_block_count();
        let mut iface = Box::new(BdIface { iface });
        let name = CString::new(name).unwrap();
        let mut raw_iface = Box::new(ext4_blockdev_iface {
            open: Some(bd_open),
            bread: Some(bd_bread),
            bwrite: Some(bd_bwrite),
            close: Some(bd_close),
            lock: None,
            unlock: None,
            ph_bsize: psz,
            ph_bcnt: pcount,
            ph_bbuf: buf.as_mut_ptr(),
            ph_refctr: 0,
            bread_ctr: 0,
            bwrite_ctr: 0,
            p_user: ((&mut *iface) as *mut BdIface).cast(),
        });
        let mut raw = Box::new(lwext4::ext4_blockdev {
            bdif: raw_iface.as_mut() as *mut _,
            part_offset: 0,
            part_size: u64::MAX,
            bc: null_mut(),
            lg_bsize: bsize,
            lg_bcnt: bcount,
            cache_write_back: 0,
            fs: null_mut(),
            journal: null_mut(),
        });
        let _ =
            errno_to_result(unsafe { lwext4::ext4_device_register(raw.as_mut(), name.as_ptr()) })?;
        Ok(Self {
            iface,
            raw,
            buf,
            raw_iface,
            name,
        })
    }
}

pub struct Ext4Fs {
    bd: Ext4Blockdev,
    mnt_name: CString,
}

pub struct Ext4File {
    file: Box<ext4_file>,
    name: CString,
}

impl Ext4File {
    pub fn len(&mut self) -> u64 {
        unsafe { lwext4::ext4_fsize(self.file.as_mut()) }
    }
}

impl Drop for Ext4File {
    fn drop(&mut self) {
        unsafe { lwext4::ext4_fclose(self.file.as_mut()) };
    }
}

pub use lwext4::{O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};

impl Ext4Fs {
    pub fn new(bd: Ext4Blockdev, mnt_name: CString, read_only: bool) -> Result<Self> {
        println!("A");
        let r = unsafe { ext4_mount(bd.name.as_ptr(), mnt_name.as_ptr(), read_only) };
        println!("{}", unsafe { (*bd.raw.bdif).ph_refctr });
        errno_to_result(r)?;
        println!("B");
        unsafe { lwext4::ext4_recover(mnt_name.as_ptr()) };
        println!("C");
        errno_to_result(unsafe { lwext4::ext4_journal_start(mnt_name.as_ptr()) })?;
        println!("D");
        Ok(Self { bd, mnt_name })
    }

    pub fn open_file(&mut self, name: &str, flags: u32) -> Result<Ext4File> {
        let name = format!("{}{}", self.mnt_name.to_string_lossy(), name);
        let name = CString::new(name).unwrap();

        let mut file = Box::new(ext4_file {
            mp: null_mut(),
            inode: 0,
            flags: 0,
            fsize: 0,
            fpos: 0,
        });
        errno_to_result(unsafe {
            lwext4::ext4_fopen2(file.as_mut(), name.as_ptr(), flags as i32)
        })?;
        Ok(Ext4File { file, name })
    }

    pub fn remove_file(&mut self, name: &str) -> Result<()> {
        let name = format!("{}{}", self.mnt_name.to_string_lossy(), name);
        let path = CString::new(name).unwrap();
        errno_to_result(unsafe { lwext4::ext4_fremove(path.as_ptr()) })
    }

    pub fn create_dir(&mut self, name: &str) -> Result<()> {
        let name = format!("{}{}", self.mnt_name.to_string_lossy(), name);
        println!("==> {}", name);
        let path = CString::new(name).unwrap();
        errno_to_result(unsafe { lwext4::ext4_dir_mk(path.as_ptr()) })
    }
}

impl Read for Ext4File {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut completed = 0;
        let r = unsafe {
            lwext4::ext4_fread(
                self.file.as_mut(),
                buf.as_mut_ptr().cast(),
                buf.len(),
                &mut completed,
            )
        };
        errno_to_result(r)?;
        Ok(completed)
    }
}

impl Write for Ext4File {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let mut completed = 0;
        let r = unsafe {
            lwext4::ext4_fwrite(
                self.file.as_mut(),
                buf.as_ptr().cast(),
                buf.len(),
                &mut completed,
            )
        };
        errno_to_result(r)?;
        Ok(completed)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

impl Seek for Ext4File {
    fn seek(&mut self, pos: std::io::SeekFrom) -> Result<u64> {
        let (mode, off) = match pos {
            std::io::SeekFrom::Start(off) => (SEEK_SET, off as i64),
            std::io::SeekFrom::End(off) => (SEEK_END, off),
            std::io::SeekFrom::Current(off) => (SEEK_CUR, off),
        };
        let r = unsafe { lwext4::ext4_fseek(self.file.as_mut(), off, mode) };
        errno_to_result(r)?;
        let pos = unsafe { lwext4::ext4_ftell(self.file.as_mut()) };
        Ok(pos)
    }
}

impl Drop for Ext4Fs {
    fn drop(&mut self) {
        unsafe { lwext4::ext4_journal_stop(self.mnt_name.as_ptr()) };
        unsafe { lwext4::ext4_umount(self.mnt_name.as_ptr()) };
    }
}
