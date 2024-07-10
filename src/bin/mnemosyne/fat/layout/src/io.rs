pub trait IO {
    type Error;
}

pub trait Read: IO {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error>;
    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Self::Error>;
}

pub trait Write: IO {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error>;
    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error>;
    fn flush(&mut self) -> Result<(), Self::Error>;
}

pub enum SeekFrom {
    Start(u64),
    End(i64),
    Current(i64),
}

pub trait Seek: IO {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error>;

    // taken from std::io::Seek's provided definition for the corollary function 
    fn stream_len(&mut self) -> Result<u64, Self::Error> {
        let old_pos = self.stream_position()?;
        let len = self.seek(SeekFrom::End(0))?;

        if old_pos != len {
            self.seek(SeekFrom::Start(old_pos))?;
        }

        Ok(len)
    }

    // taken from std::io::Seek's provided definition for the corollary function 
    fn stream_position(&mut self) -> Result<u64, Self::Error> {
        self.seek(SeekFrom::Current(0))
    }
}

pub struct StdIO<S>(pub S);

impl<S> IO for StdIO<S> {
    type Error = std::io::Error;
}

impl<S: std::io::Read> Read for StdIO<S> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.read(buf)
    }

    fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read_exact(buf)
    }
}

impl<S: std::io::Write> Write for StdIO<S> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.0.write(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> Result<(), Self::Error> {
        self.0.write_all(buf)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush()
    }
}

impl<S: std::io::Seek> Seek for StdIO<S> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let pos = match pos {
            SeekFrom::Start(o) => std::io::SeekFrom::Start(o),
            SeekFrom::End(o) => std::io::SeekFrom::End(o),
            SeekFrom::Current(o) => std::io::SeekFrom::Current(o),
        };

        self.0.seek(pos)
    }

    fn stream_position(&mut self) -> Result<u64, Self::Error> {
        self.0.stream_position()
    }
}
