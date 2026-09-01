fn main() {
    println!("devfs_test: run the #[test]s via the test suite");
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    #[test]
    fn null_read_is_eof() {
        let mut f = std::fs::File::open("/dev/null").unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(f.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn null_write_is_discarded() {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .unwrap();
        assert_eq!(f.write(b"hello").unwrap(), 5);
    }

    // std's create() maps to CreateKindEither: it must open the existing node, not try to
    // create over it.
    #[test]
    fn null_open_with_create_flag() {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open("/dev/null")
            .unwrap();
        assert_eq!(f.write(b"x").unwrap(), 1);
    }

    #[test]
    fn dev_lists_all_nodes() {
        let names: Vec<_> = std::fs::read_dir("/dev")
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        for want in [
            "null", "zero", "urandom", "tty", "stdin", "stdout", "stderr",
        ] {
            assert!(names.iter().any(|n| n == want), "no {want} in {names:?}");
        }
    }

    #[test]
    fn zero_reads_zeros() {
        let mut f = std::fs::File::open("/dev/zero").unwrap();
        let mut buf = [0xffu8; 32];
        assert_eq!(f.read(&mut buf).unwrap(), 32);
        assert!(buf.iter().all(|b| *b == 0));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/zero")
            .unwrap();
        assert_eq!(f.write(b"xyz").unwrap(), 3);
    }

    #[test]
    fn urandom_reads() {
        let mut f = std::fs::File::open("/dev/urandom").unwrap();
        let mut buf = [0u8; 32];
        f.read_exact(&mut buf).unwrap();
        // All-zero from a healthy CSPRNG is a 2^-256 event; treat it as failure.
        assert!(buf.iter().any(|b| *b != 0));
    }

    #[test]
    fn stdio_nodes_alias_open_descriptors() {
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/stdout")
            .unwrap();
        assert!(out.write(b"devfs_test: hello via /dev/stdout\n").is_ok());
        let mut err = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/stderr")
            .unwrap();
        assert!(err.write(b"devfs_test: hello via /dev/stderr\n").is_ok());
        std::fs::File::open("/dev/stdin").unwrap();
    }

    #[test]
    fn dev_is_read_only() {
        assert!(std::fs::create_dir("/dev/foo").is_err());
        assert!(std::fs::File::create("/dev/newfile").is_err());
        assert!(std::fs::remove_file("/dev/null").is_err());
        assert!(std::fs::File::open("/dev/null").is_ok());
    }

    #[test]
    fn null_metadata() {
        let md = std::fs::metadata("/dev/null").unwrap();
        assert!(!md.is_dir());
        assert_eq!(md.len(), 0);
    }
}
