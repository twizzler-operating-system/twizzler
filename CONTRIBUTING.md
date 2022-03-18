# Contributions to Twizzler

If you want to help Twizzler, wonderful! We'd love to have you. To find a place to start, we have a [list of issues outlined](https://github.com/twizzler-operating-system/twizzler/issues). For newer programmers, easier issues are tagged [good first issue](https://github.com/twizzler-operating-system/twizzler/labels/good%20first%20issue) and are a good place to dive in.

## Writing code

To get started, create a fork of the repository and clone it locally. You can then write your code to fix the issue. We ask that in order to make consistent, understandable code, you follow these guidelines:
- Follow the Rust style guide. This is just the default style for all rust code established by `rustfmt`, explained at [this GitHub repository](https://github.com/rust-lang/rustfmt#readme).
- Be well documented. Please add documentation to explain your code. For more information on this, see the [section on documentation](#documentation).
- For unsafe code, have special safety documentation to explain why the code must be unsafe, and extra scrutiny on that code to ensure its correctness.
- Pass [clippy](https://github.com/rust-lang/rust-clippy#readme), the rust linter. It is useful for finding common mistakes and improving code.

## Adding your code to the main repository

To add your code to public Twizzler, create a [pull request](https://github.com/twizzler-operating-system/twizzler/pulls). We will review it and merge it if it looks good, or ask for specific fixes if we think you are missing something.

Thanks for your contributions!

## Documentation

If you want to document code instead of writing it, thank you! Rust has a built-in documentation tool `rustdoc` which makes compiling documentation easier, and writing it directly in the code more natural. [Documentation explanation](https://doc.rust-lang.org/cargo/index.html). Contributions can be made through pull requests, just like for code, explained in [this section](#adding-your-code-to-the-main-repository).
