# Developing for Twizzler

The Twizzler project welcomes people to contribute to this open source
operating system.  We follow a branch and pull-request process.  For
those without write access to the repo you will first have to fork the
Twizzler repo, for those with write access it's easier to work within
the main repo in your own branches.

## Writing your First Twizzler Program

Twizzler makes heavy use of Rust's Cargo system of crates in order to manage both kernel and user space programs.  To create a new program that
runs on the Twizzler operating system you can follow these steps:

1. Change directories into the bin directory:

```cd twizzler/src/bin```

2. Create a new program, named ```hello```.  The ```Cargo``` command will actually create the canonical ```Hello world``` program for you.
```cargo new --bin hello```

3. Change directory back to the project root, and edit the Cargo.toml file in the root directory to add the program to the Twizzler build system.

The following diff shows the two lines you need to add:

```diff
diff --git a/Cargo.toml b/Cargo.toml
index 338455b..cb38fb5 100644
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -7,6 +7,7 @@ members = [
     "src/bin/devmgr",
     "src/bin/netmgr",
     "src/bin/nettest",
+    "src/bin/hello",
     "src/kernel",
     "src/lib/twizzler-queue-raw",
     "src/lib/twizzler-queue",
@@ -21,10 +22,11 @@ initrd = [
     "crate:devmgr",
     "crate:netmgr",
     "crate:nettest",
+    "crate:hello",
 ]
 ```

   4. Rebuild the system and start QEMU

```cargo start-qemu```

  5. From within Twizzler run your new program

```
run hello
got: <run hello>
> Hello, world!
```

  6. Exit the simulator with ```Ctrl-a x```

## Picking an Issue to Work

Before reading this document make sure that you have worked through the build process, which is documented
[here](https://github.com/twizzler-operating-system/twizzler/doc/src/BUILD.md).

After reading this document, if you would like to work on an open issue,
you can take a look at a
[list of issues](https://github.com/twizzler-operating-system/twizzler/issues).
For newer contributors, easier issues are tagged
[good first issue](https://github.com/twizzler-operating-system/twizzler/labels/good%20first%20issue)
and are a good place to dive in.


## Branch Naming

Branch names should be short and descriptive, containing the user's
github or other short name, following by a dash and then a feature
name, e.g. gnn-icmp.  Names must not violate the Twizzler project's
Code of Conduct.  Please keep it classy.

## Submitting a Pull Request

All pull requests should be against the 'main' branch.  From time to
time there may be special exceptions but these must be coordinated
with the project owners, listed on the main github page.

## Example Workflow

In order to create this set of documentation the following steps were
carried out.

```
> git clone git@github.com:twizzler-operating-system/twizzler.git
```

Create and edit file in doc/src/develop.md

```
> git branch -b gnn-docs
> git add doc/src/develop.md
> git commit
> git push --set-upstream origin gnn-docs
```

The pull request was then submitted from the github page for the
Twizzer project.

Two reviewers were added at the time the PR was committed.


# Coding Standards

Code submitted to Twizzler must follow these guidelines:

- Follow the Rust style guide. This is just the default style for all rust code established by `rustfmt`, explained at [this GitHub repository](https://github.com/rust-lang/rustfmt#readme).
- Be well documented. Please add documentation to explain your code. For more information on this, see below.
- For unsafe code, have special safety documentation to explain why the code must be unsafe, and extra scrutiny on that code to ensure its correctness.
- Pass [clippy](https://github.com/rust-lang/rust-clippy#readme), the rust linter. It is useful for finding common mistakes and improving code.

## Documentation

If you want to document code instead of writing it, thank you! Rust has a built-in documentation tool `rustdoc` which makes compiling documentation easier, and writing it directly in the code more natural. [Documentation explanation](https://doc.rust-lang.org/cargo/index.html). Contributions can be made through pull requests, just like for code, explained above.

## Testing

Twizzler supports an integrated testing system, where kernel test harnesses and userspace test
programs for libraries and binaries are run on start-up. You can run Twizzler in test mode by adding
the `--tests` argument to your cargo run commands.

When developing for Twizzler, you should write tests that cover code you wrote. This includes both
userspace and kernelspace code. Note that writing tests for the kernel is slightly different, in
that a test-case failing causes the whole system to stop, and you need to use the `#[kernel_test]`
attribute from the `twizzler-kernel-macros` crate instead of `#[test]`.
