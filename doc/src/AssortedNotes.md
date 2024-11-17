# Assorted Notes

## Updating the toolchain

1. Update the repos. Go into toolchain/src/rust and, for each of ., library/libc, library/backtrace, and src/llvm-project, you'll need to:
    a. Add the upstream remote (usually github rust-lang/<name of repo>, though backtrace is called "backtrace-rs" there)
    b. Ensure you've fetched the twizzler branch from origin, and the following branches from upstream:
        - libc: "libc-2.0"
        - bracktrace: "master"
        - llvm-project: you'll need to look at the upstream repo and determine the latest version number, looking at the branches on github.
        - .: master
2. Checkout master for rust (.), ensure that this checks out the new submodule commits too.
3. Rebase the submodules. For each of libc, backtrace, and llvm-project, you'll need to go into that directory and rebase the twizzler branch onto the current commit.
4. Rebase the twizzler branch onto master in the rust repo. There will probably be conflicts and compile errors.
5. Commit all the submodules, push, and do the same for the rust repo.
6. Test...
Done!
