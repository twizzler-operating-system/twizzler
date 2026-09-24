fn main() {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(0, &mut st) } != 0 {
        println!("PIPESTAT fstat-failed errno={}", unsafe {
            *libc::__errno_location()
        });
        return;
    }
    let kind = match st.st_mode & libc::S_IFMT {
        libc::S_IFIFO => "FIFO",
        libc::S_IFCHR => "CHR",
        libc::S_IFREG => "REG",
        libc::S_IFDIR => "DIR",
        libc::S_IFSOCK => "SOCK",
        other => {
            println!("PIPESTAT mode={:o} kind=UNKNOWN({:o})", st.st_mode, other);
            return;
        }
    };
    println!("PIPESTAT mode={:o} kind={}", st.st_mode, kind);
}
