use super::{DataSync, Io, Read, Seek, SeekFrom, Write};

pub struct StdIo<T> {
    inner: T,
}

impl<T> StdIo<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &T {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    pub fn to_inner(self) -> T {
        self.inner
    }
}

impl<T> Io for StdIo<T> {
    type Error = std::io::Error;
}

impl<T: std::io::Read> Read for StdIo<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.inner.read(buf)
    }
}

impl<T: std::io::Write> Write for StdIo<T> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

impl From<SeekFrom> for std::io::SeekFrom {
    fn from(value: SeekFrom) -> Self {
        match value {
            SeekFrom::Start(n) => std::io::SeekFrom::Start(n),
            SeekFrom::End(n) => std::io::SeekFrom::End(n),
            SeekFrom::Current(n) => std::io::SeekFrom::Current(n),
        }
    }
}

impl<T: std::io::Seek> Seek for StdIo<T> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        self.inner.seek(pos.into())
    }
}

impl<T: DataSync + Io<Error = std::io::Error>> DataSync for StdIo<T> {
    fn sync_all(&self) -> Result<(), Self::Error> {
        self.inner.sync_all()
    }

    fn sync_data(&self) -> Result<(), Self::Error> {
        self.inner.sync_data()
    }
}

impl DataSync for StdIo<std::fs::File> {
    fn sync_all(&self) -> Result<(), Self::Error> {
        self.inner.sync_all()
    }

    fn sync_data(&self) -> Result<(), Self::Error> {
        self.inner.sync_data()
    }
}

// impl<IO> DataSync for StdIo<syncfile::File<'_, IO>>
// where
//     IO: ReadWriteSeek,
// {
//     fn sync_all(&self) -> Result<(), Self::Error> {
//         self.sync_all();
//         self.inner.sync_all()
//     }

//     fn sync_data(&self) -> Result<(), Self::Error> {
//         self.sync_data()
//     }
// }
