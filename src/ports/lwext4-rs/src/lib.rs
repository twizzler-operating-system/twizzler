use std::{
    ffi::{c_void, CString},
    io::{Read, Result, Seek, Write},
    mem::MaybeUninit,
    ptr::null_mut,
    sync::{Condvar, Mutex},
    u64,
};

use lwext4::{
    ext4_block, ext4_blockdev, ext4_blockdev_iface, ext4_dir_iterator_fini, ext4_dir_iterator_init,
    ext4_dir_iterator_next, ext4_extent_get_blocks, ext4_file, ext4_fs_init_inode_dblk_idx,
    ext4_fs_insert_inode_dblk, ext4_fs_put_inode_ref, ext4_fsblk_t, ext4_get_mount,
    ext4_inode_get_size, ext4_inode_ref, ext4_mount, EOK, SEEK_CUR, SEEK_END, SEEK_SET,
};

#[allow(unused, nonstandard_style)]
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

unsafe extern "C" fn bd_lock(bd: *mut ext4_blockdev) -> i32 {
    let iface = get_iface(bd);
    result_to_errno(iface.lock())
}

unsafe extern "C" fn bd_unlock(bd: *mut ext4_blockdev) -> i32 {
    let iface = get_iface(bd);
    result_to_errno(iface.unlock())
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
    fn lock(&self) -> Result<()>;
    fn unlock(&self) -> Result<()>;
}

struct BdIface {
    iface: Box<dyn Ext4BlockdevIface>,
}

#[allow(dead_code, unused_variables)]
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
    pub fn iface(&mut self) -> &mut Box<dyn Ext4BlockdevIface> {
        &mut self.iface.iface
    }

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
            lock: Some(bd_lock),
            unlock: Some(bd_unlock),
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

#[allow(dead_code, unused_variables)]
pub struct Ext4Fs {
    bd: Ext4Blockdev,
    mnt_name: CString,
}

#[allow(dead_code, unused_variables)]
pub struct Ext4File<'a> {
    file: Box<ext4_file>,
    name: CString,
    fs: &'a mut Ext4Fs,
}

impl Ext4File<'_> {
    pub fn len(&mut self) -> u64 {
        unsafe { lwext4::ext4_fsize(self.file.as_mut()) }
    }

    pub fn truncate(&mut self, new_size: u64) -> Result<()> {
        errno_to_result(unsafe { lwext4::ext4_ftruncate(self.file.as_mut(), new_size) })
    }

    pub fn ensure_backing(&mut self, offset: u64) -> Result<()> {
        let mut inode = self.fs.get_inode(self.file.inode)?;
        let block_size = self.fs.block_size()?;
        inode.get_data_block((offset / block_size) as u32, true)?;
        Ok(())
    }

    pub fn get_file_inode(&mut self) -> Result<Ext4InodeRef> {
        self.fs.get_inode(self.file.inode)
    }
}

impl Drop for Ext4File<'_> {
    fn drop(&mut self) {
        unsafe { lwext4::ext4_fclose(self.file.as_mut()) };
    }
}

pub use lwext4::{O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};

pub struct Ext4InodeRef {
    inode: ext4_inode_ref,
}

pub enum FileKind {
    Regular,
    Directory,
    Symlink,
    Other,
}

impl Ext4InodeRef {
    pub fn get_data_block(&mut self, block: u32, create: bool) -> Result<u64> {
        let mut fblock = 0;
        if create {
            errno_to_result(unsafe {
                ext4_fs_insert_inode_dblk(&mut self.inode, &mut fblock, block)
            })?;
        } else {
            errno_to_result(unsafe {
                ext4_fs_init_inode_dblk_idx(&mut self.inode, block, &mut fblock)
            })?;
        }
        Ok(fblock)
    }

    pub fn get_data_blocks(
        &mut self,
        block: u32,
        max_blocks: u32,
        create: bool,
    ) -> Result<(ext4_fsblk_t, u32)> {
        let mut count = 0;
        let mut dblock = 0;
        errno_to_result(unsafe {
            ext4_extent_get_blocks(
                &mut self.inode,
                block,
                max_blocks,
                &mut dblock,
                create,
                &mut count,
            )
        })?;

        Ok((dblock, count))
    }

    pub fn num(&self) -> u32 {
        self.inode.index
    }

    pub fn size(&self) -> u64 {
        unsafe { ext4_inode_get_size(&mut (*self.inode.fs).sb, self.inode.inode) }
    }

    pub fn kind(&self) -> FileKind {
        if unsafe {
            lwext4::ext4_inode_is_type(
                &mut (*self.inode.fs).sb,
                self.inode.inode,
                lwext4::EXT4_INODE_MODE_FILE,
            )
        } {
            return FileKind::Regular;
        }
        if unsafe {
            lwext4::ext4_inode_is_type(
                &mut (*self.inode.fs).sb,
                self.inode.inode,
                lwext4::EXT4_INODE_MODE_DIRECTORY,
            )
        } {
            return FileKind::Directory;
        }
        if unsafe {
            lwext4::ext4_inode_is_type(
                &mut (*self.inode.fs).sb,
                self.inode.inode,
                lwext4::EXT4_INODE_MODE_SOFTLINK,
            )
        } {
            return FileKind::Symlink;
        }
        FileKind::Other
    }
}

impl Drop for Ext4InodeRef {
    fn drop(&mut self) {
        unsafe { ext4_fs_put_inode_ref(&mut self.inode) };
    }
}

struct MpLock {
    inner: Mutex<bool>,
    cv: Condvar,
}

impl MpLock {
    fn lock(&self) {
        let mut inner = self.inner.lock().unwrap();
        while *inner {
            inner = self.cv.wait(inner).unwrap();
        }
        *inner = true;
    }

    fn unlock(&self) {
        let mut inner = self.inner.lock().unwrap();
        assert!(*inner);
        *inner = false;
        self.cv.notify_all();
    }
}

static MP_LOCK: MpLock = MpLock {
    inner: Mutex::new(false),
    cv: Condvar::new(),
};

unsafe extern "C" fn _mp_lock() {
    MP_LOCK.lock();
}

unsafe extern "C" fn _mp_unlock() {
    MP_LOCK.unlock();
}

static BC_LOCK: MpLock = MpLock {
    inner: Mutex::new(false),
    cv: Condvar::new(),
};

unsafe extern "C" fn _bc_lock() {
    BC_LOCK.lock();
}

unsafe extern "C" fn _bc_unlock() {
    BC_LOCK.unlock();
}

static BA_LOCK: MpLock = MpLock {
    inner: Mutex::new(false),
    cv: Condvar::new(),
};

unsafe extern "C" fn _ba_lock() {
    BA_LOCK.lock();
}

unsafe extern "C" fn _ba_unlock() {
    BA_LOCK.unlock();
}

static IA_LOCK: MpLock = MpLock {
    inner: Mutex::new(false),
    cv: Condvar::new(),
};

unsafe extern "C" fn _ia_lock() {
    IA_LOCK.lock();
}

unsafe extern "C" fn _ia_unlock() {
    IA_LOCK.unlock();
}

static LOCKS: lwext4::ext4_lock = lwext4::ext4_lock {
    lock: Some(_mp_lock),
    unlock: Some(_mp_unlock),
};

impl Ext4Fs {
    pub fn bd(&mut self) -> &mut Ext4Blockdev {
        &mut self.bd
    }

    pub fn new(bd: Ext4Blockdev, mnt_name: CString, read_only: bool) -> Result<Self> {
        let r = unsafe { ext4_mount(bd.name.as_ptr(), mnt_name.as_ptr(), read_only) };
        errno_to_result(r)?;
        unsafe { lwext4::ext4_recover(mnt_name.as_ptr()) };
        errno_to_result(unsafe { lwext4::ext4_journal_start(mnt_name.as_ptr()) })?;
        errno_to_result(unsafe { lwext4::ext4_mount_setup_locks(mnt_name.as_ptr(), &LOCKS) })?;

        let fs = unsafe { lwext4::ext4_mountpoint_fs(mnt_name.as_ptr()) };
        unsafe {
            (*fs).bcache_lock = Some(_bc_lock);
            (*fs).bcache_unlock = Some(_bc_unlock);

            (*fs).inode_alloc_lock = Some(_ia_lock);
            (*fs).inode_alloc_unlock = Some(_ia_unlock);

            (*fs).block_alloc_lock = Some(_ba_lock);
            (*fs).block_alloc_unlock = Some(_ba_unlock);
        }

        Ok(Self { bd, mnt_name })
    }

    pub fn dirents(&mut self, inode: &mut Ext4InodeRef) -> Result<DirIter> {
        let mut this = DirIter {
            it: unsafe { MaybeUninit::zeroed().assume_init() },
            done: false,
            fs: self,
        };
        errno_to_result(unsafe { ext4_dir_iterator_init(&mut this.it, &mut inode.inode, 0) })?;
        Ok(this)
    }

    pub fn open_file(&mut self, name: &str, flags: u32) -> Result<Ext4File<'_>> {
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
        Ok(Ext4File {
            file,
            name,
            fs: self,
        })
    }

    pub fn open_file_from_inode(&mut self, index: u32, flags: u32) -> Result<Ext4File<'_>> {
        let name = format!("{}#{}", self.mnt_name.to_string_lossy(), index);
        let name = CString::new(name).unwrap();

        let inode = self.get_inode(index)?;
        let file = Box::new(ext4_file {
            mp: unsafe { ext4_get_mount(self.mnt_name.as_ptr()) },
            inode: inode.num(),
            flags,
            fsize: inode.size(),
            fpos: 0,
        });
        Ok(Ext4File {
            file,
            name,
            fs: self,
        })
    }

    pub fn get_inode(&mut self, index: u32) -> Result<Ext4InodeRef> {
        let fs = unsafe { lwext4::ext4_mountpoint_fs(self.mnt_name.as_ptr()) };

        let mut inode = ext4_inode_ref {
            block: ext4_block {
                lb_id: 0,
                buf: null_mut(),
                data: null_mut(),
            },
            inode: null_mut(),
            fs: null_mut(),
            index,
            dirty: false,
        };
        errno_to_result(unsafe { lwext4::ext4_fs_get_inode_ref(fs, index, &mut inode) })?;
        Ok(Ext4InodeRef { inode })
    }

    pub fn block_size(&mut self) -> Result<u64> {
        let mut sb = null_mut();
        errno_to_result(unsafe { lwext4::ext4_get_sblock(self.mnt_name.as_ptr(), &mut sb) })?;
        let block_size = 1024 << (unsafe { *sb }).log_block_size;
        Ok(block_size)
    }

    pub fn remove_file(&mut self, name: &str) -> Result<()> {
        let name = format!("{}{}", self.mnt_name.to_string_lossy(), name);
        let path = CString::new(name).unwrap();
        errno_to_result(unsafe { lwext4::ext4_fremove(path.as_ptr()) })
    }

    pub fn create_dir(&mut self, name: &str) -> Result<()> {
        let name = format!("{}{}", self.mnt_name.to_string_lossy(), name);
        let path = CString::new(name).unwrap();
        errno_to_result(unsafe { lwext4::ext4_dir_mk(path.as_ptr()) })
    }
}

impl Read for Ext4File<'_> {
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

impl Write for Ext4File<'_> {
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

impl Seek for Ext4File<'_> {
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

pub struct DirIter<'a> {
    it: lwext4::ext4_dir_iter,
    done: bool,
    fs: &'a mut Ext4Fs,
}

impl<'a> Drop for DirIter<'a> {
    fn drop(&mut self) {
        unsafe { ext4_dir_iterator_fini(&mut self.it) };
    }
}

impl<'a> Iterator for DirIter<'a> {
    type Item = (Vec<u8>, Result<Ext4InodeRef>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.it.curr.is_null() {
            return None;
        }
        let item = unsafe { &*self.it.curr };
        let name = unsafe { item.name.as_slice(item.name_len as usize) }.to_vec();
        let next = self.fs.get_inode(item.inode);
        if unsafe { ext4_dir_iterator_next(&mut self.it) } != EOK as i32 {
            self.done = true;
        }
        Some((name, next))
    }
}
