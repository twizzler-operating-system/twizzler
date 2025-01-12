use std::fmt::Debug;
use std::{
    fs::{FileTimes, Metadata, Permissions},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd},
        // unix::fs::FileExt,
    },
    path::Path,
    process::Stdio,
    time::SystemTime,
};

use fatfs::{
    DefaultTimeProvider, Error, File as StdFile, FileSystem, IoBase, LossyOemCpConverter, Read,
    ReadWriteSeek, Seek, SeekFrom, Write,
};

// type Result<T> = std::result::Result<T, Error<std::io::Error>>;

type Tp = DefaultTimeProvider;
type Occ = LossyOemCpConverter;

type FsResult<T, IO> = std::result::Result<T, fatfs::Error<<IO as IoBase>::Error>>;

pub struct File<'a, IO: fatfs::ReadWriteSeek + 'a>(StdFile<'a, IO, Tp, Occ>);

impl<'a, IO> File<'a, IO>
where
    IO: fatfs::ReadWriteSeek + 'static,
{
    pub fn create<'b>(fs: &'b mut fatfs::FileSystem<IO, Tp, Occ>, path: &str) -> FsResult<Self, IO>
    where
        'b: 'a, // where b outlives a
    {
        let file = fs.root_dir().create_file(path)?;
        Ok(Self(file))
    }

    pub fn create_new<'b>(
        _fs: &'b mut fatfs::FileSystem<IO, Tp, Occ>,
        _path: impl AsRef<Path>,
    ) -> FsResult<Self, IO>
    where
        'b: 'a,
    {
        // fs.root_dir().create_new(path).map(Self)
        unimplemented!()
    }

    pub fn metadata(&self) -> FsResult<Metadata, IO> {
        unimplemented!()
    }

    pub fn open<'b>(fs: &'b mut fatfs::FileSystem<IO, Tp, Occ>, path: &str) -> FsResult<Self, IO>
    where
        'b: 'a,
    {
        let file = fs.root_dir().open_file(path)?;
        Ok(Self(file))
    }

    pub fn options(fs: &'a FileSystem<IO, Tp, Occ>) -> OpenOptions<'a, IO> {
        OpenOptions::new(fs)
    }

    pub fn set_len(&mut self, size: u64) -> FsResult<(), IO> {
        self.0.seek(SeekFrom::Start(size))?;
        self.0.truncate()
    }

    pub fn set_modified(&self, _time: SystemTime) -> FsResult<(), IO> {
        unimplemented!()
    }

    pub fn set_permissions(&self, _perm: Permissions) -> FsResult<(), IO> {
        unimplemented!()
    }

    pub fn set_times(&self, _times: FileTimes) -> FsResult<(), IO> {
        unimplemented!()
    }

    pub fn sync_all(&mut self) -> FsResult<(), IO> {
        // unimplemented!()
        self.0.flush()
        // let fd = self.0.as_raw_fd();
        // if unsafe { libc::fsync(fd) } < 0 {
        //     Err(io::Error::last_os_error())
        // } else {
        //     Ok(())
        // }
    }

    pub fn sync_data(&mut self) -> FsResult<(), IO> {
        // let fd = self.0.as_raw_fd();
        // if unsafe { libc::fdatasync(fd) } < 0 {
        //     Err(io::Error::last_os_error())
        // } else {
        //     Ok(())
        // }
        self.0.flush() // should do something similar to fdatasync?
    }

    pub fn try_clone(&self) -> FsResult<Self, IO> {
        Ok(Self(self.0.clone()))
    }
}

impl<'a, IO> AsFd for File<'a, IO>
where
    IO: ReadWriteSeek,
{
    fn as_fd(&self) -> BorrowedFd<'_> {
        unimplemented!()
    }
}

impl<'a, IO> AsRawFd for File<'a, IO>
where
    IO: ReadWriteSeek,
{
    fn as_raw_fd(&self) -> RawFd {
        unimplemented!()
    }
}

// impl<'a, IO> FileExt for File<'a, IO>
// where
//     IO: ReadWriteSeek,
//     std::io::Error: From<Error<IO::Error>>,
// {
//     fn read_at(&self, _buf: &mut [u8], _offset: u64) -> Result<usize, std::io::Error> {
//         unimplemented!()
//         // let current_pos = self.0.seek(SeekFrom::Current(0))?;
//         // self.0.seek(SeekFrom::Start(offset))?;
//         // let out = self.0.read(buf)?;
//         // self.0.seek(SeekFrom::Start(current_pos))?;
//         // Ok(out)
//     }

//     fn write_at(&self, _buf: &[u8], _offset: u64) -> Result<usize, std::io::Error> {
//         unimplemented!()
//         // let current_pos = self.0.seek(SeekFrom::Current(0))?;
//         // self.0.seek(SeekFrom::Start(offset))?;
//         // let out = self.0.write(buf)?;
//         // self.0.seek(SeekFrom::Start(current_pos))?;
//         // Ok(out)
//     }
// }

impl<'a, IO> From<File<'a, IO>> for OwnedFd
where
    IO: ReadWriteSeek,
{
    fn from(_value: File<'a, IO>) -> Self {
        unimplemented!()
    }
}

impl<'a, IO> From<File<'a, IO>> for Stdio
where
    IO: ReadWriteSeek,
{
    fn from(_value: File<'a, IO>) -> Self {
        unimplemented!()
    }
}

impl<'a, IO> From<OwnedFd> for File<'a, IO>
where
    IO: ReadWriteSeek,
{
    fn from(_value: OwnedFd) -> Self {
        unimplemented!()
    }
}

impl<'a, IO> FromRawFd for File<'a, IO>
where
    IO: ReadWriteSeek,
{
    unsafe fn from_raw_fd(_fd: RawFd) -> Self {
        unimplemented!()
    }
}

impl<'a, IO> IntoRawFd for File<'a, IO>
where
    IO: ReadWriteSeek,
{
    fn into_raw_fd(self) -> RawFd {
        unimplemented!()
    }
}

impl<'a, IO> std::io::Read for &File<'a, IO>
where
    IO: ReadWriteSeek,
    std::io::Error: From<Error<IO::Error>>,
{
    fn read(&mut self, _buf: &mut [u8]) -> Result<usize, std::io::Error> {
        // Ok(fatfs::Read::read(&mut self.0, buf)?)
        unimplemented!()
    }
}

impl<'a, IO> std::io::Read for File<'a, IO>
where
    IO: ReadWriteSeek,
    std::io::Error: From<Error<IO::Error>>,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        let res = Read::read(&mut self.0, buf);
        // let res = match res {
        //     Ok(v) => {
        //         if v == 0 {
        //             Err(std::io::Error::new(
        //                 std::io::ErrorKind::WriteZero,
        //                 "Read zero bytes.",
        //             ))
        //         } else {
        //             Ok(v)
        //         }
        //     }

        //     Err(e) => Err(std::io::Error::from(e)),
        // };
        Ok(res?)
    }
}

impl<'a, IO> std::io::Seek for &File<'a, IO>
where
    IO: ReadWriteSeek,
{
    fn seek(&mut self, _pos: std::io::SeekFrom) -> Result<u64, std::io::Error> {
        // Seek::seek(&mut &self.0, pos)
        unimplemented!()
    }
}

impl<'a, IO> std::io::Seek for File<'a, IO>
where
    IO: ReadWriteSeek,
    std::io::Error: From<Error<IO::Error>>,
{
    fn seek(&mut self, pos: std::io::SeekFrom) -> Result<u64, std::io::Error> {
        Ok(fatfs::Seek::seek(&mut self.0, pos.into())?)
    }
}

impl<'a, IO> std::io::Write for &File<'a, IO>
where
    IO: ReadWriteSeek,
{
    fn write(&mut self, _buf: &[u8]) -> Result<usize, std::io::Error> {
        unimplemented!()
        // Write::write(&mut self.0, buf).map_err(|e| std::io::Error::other(format!("{e:?}")))
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        unimplemented!()
        // Write::flush(&mut &self.0)
    }
}

impl<'a, IO> std::io::Write for File<'a, IO>
where
    IO: ReadWriteSeek,
    std::io::Error: From<Error<IO::Error>>,
{
    fn write(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        Ok(fatfs::Write::write(&mut self.0, buf)?)
    }

    fn flush(&mut self) -> Result<(), std::io::Error> {
        Ok(fatfs::Write::flush(&mut self.0)?)
    }
}

pub struct OpenOptions<'a, IO: ReadWriteSeek> {
    append: bool,
    create: bool,
    create_new: bool,
    read: bool,
    truncate: bool,
    write: bool,
    fs: &'a FileSystem<IO, Tp, Occ>,
}

impl<'a, IO> Clone for OpenOptions<'a, IO>
where
    IO: ReadWriteSeek,
{
    fn clone(&self) -> Self {
        Self {
            append: self.append.clone(),
            create: self.create.clone(),
            create_new: self.create_new.clone(),
            read: self.read.clone(),
            truncate: self.truncate.clone(),
            write: self.write.clone(),
            fs: self.fs,
        }
    }
}

impl<'a, IO> Debug for OpenOptions<'a, IO>
where
    IO: ReadWriteSeek,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenOptions")
            .field("append", &self.append)
            .field("create", &self.create)
            .field("create_new", &self.create_new)
            .field("read", &self.read)
            .field("truncate", &self.truncate)
            .field("write", &self.write)
            .finish()
    }
}

impl<'a, IO> OpenOptions<'a, IO>
where
    IO: ReadWriteSeek,
{
    pub fn append(&mut self, append: bool) -> &mut Self {
        self.append = append;
        self.truncate = !append;
        self
    }

    pub fn create(&mut self, create: bool) -> &mut Self {
        self.create = create;
        self
    }

    pub fn create_new(&mut self, create_new: bool) -> &mut Self {
        self.create_new = create_new;
        self
    }

    pub fn new(fs: &'a FileSystem<IO, Tp, Occ>) -> Self {
        Self {
            append: true,
            create: false,
            create_new: false,
            read: false,
            truncate: false,
            write: false,
            fs,
        }
    }

    pub fn open(&self, path: &str) -> FsResult<File<'a, IO>, IO>
    where
        IO: ReadWriteSeek,
    {
        let mut out = if self.create_new {
            unimplemented!()
        } else if self.create {
            self.fs.root_dir().create_file(&path)
        } else {
            self.fs.root_dir().open_file(&path)
        }?;
        // default is append
        if self.truncate != !self.append {
            panic!("Truncate should only be true if not appending.")
        }
        if self.truncate && !self.append {
            out.truncate()?;
        }

        if self.write {
            // doesn't do anything since fatfs files are always read+write
        }

        Ok(File(out))
    }

    pub fn read(&mut self, _read: bool) -> &mut Self {
        self.read = true;
        self
    }

    pub fn truncate(&mut self, truncate: bool) -> &mut Self {
        self.truncate = truncate;
        self.append = !truncate;
        self
    }

    pub fn write(&mut self, write: bool) -> &mut Self {
        self.write = write;
        self
    }
}

// impl OpenOptionsExt for OpenOptions {
//     fn mode(&mut self, mode: u32) -> &mut Self {
//         self.0.mode(mode);
//         self
//     }

//     fn custom_flags(&mut self, flags: i32) -> &mut Self {
//         self.0.custom_flags(flags);
//         self
//     }
// }
