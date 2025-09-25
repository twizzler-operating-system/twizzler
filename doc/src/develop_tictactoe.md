
# Develop Tic Tac Toe! 

This project implements a simple terminal-based Tic Tac Toe game in Rust using:

- [`noline`](https://docs.rs/noline): A minimal line editor for user input.
- [`embedded-io`](https://docs.rs/embedded-io): Trait abstractions for embedded-style I/O.
- Rust standard I/O wrapped for compatibility.

---

## Project Setup

1. **Create the project**

Enter `src/bin` and create the project.
```bash
cargo new tictactoe
cd tictactoe
```

2. **Add dependencies**

Edit your `Cargo.toml` in the project (`src/bin/tictactoe/Cargo.toml`) to include the newest versions of noline and embedded-io, and the std feature for embedded-io:

```toml
[dependencies]
noline = "*"
embedded-io = { version = "*", features = ["std"] }
```

Add the dependencies section if it is not there already

3. **Replace the contents of `tictactoe/src/main.rs`** with the code in the next section.

4. **ADD TICTACTOE TO `Cargo.toml`**

Change directory back to the project root, and edit the `Cargo.toml` file in the root directory to add the program to the Twizzler build system.
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
+    "src/bin/tictactoe",
     "src/kernel",
     "src/lib/twizzler-queue-raw",
     "src/lib/twizzler-queue",
@@ -21,10 +22,11 @@ initrd = [
     "crate:devmgr",
     "crate:netmgr",
     "crate:nettest",
+    "crate:tictactoe",
 ]

```

---

## Code Overview

Below is a breakdown of how the code works.

---

1. Imports and I/O Adapter Setup

```rust
use std::io::{self, Read as StdRead, Write as StdWrite};
use noline::builder::EditorBuilder;
use embedded_io::{ErrorType, Read, Write};
```

- Use standard I/O for input/output.
- Bring in `EditorBuilder` from `noline` to manage user input.
- Use `embedded-io` traits to define how input/output behaves.

---

2. Bridging I/O: `TwzIo`

We implement a struct that conforms to the traits expected by `noline` from `src/bin/init/src/main.rs`.

```rust
struct TwzIo;

impl ErrorType for TwzIo {
    type Error = std::io::Error;
}

impl Read for TwzIo {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        io::stdin().read(buf)
    }
}

impl Write for TwzIo {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        io::stdout().write(buf)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        io::stdout().flush()
    }
}
```

This allows `noline` to interact with standard input/output through a compatible interface.

---

3. Main

```rust
fn main() {
    start_tictactoe();
}
```

---

4. Game Initialization

```rust
fn start_tictactoe() {
    let mut board = [[' '; 3]; 3];
    let mut current_player = 'X';
    let mut io = TwzIo;
    let mut buffer = [0; 1024];
    let mut history = [0; 1024];

    let mut editor = EditorBuilder::from_slice(&mut buffer)
        .with_slice_history(&mut history)
        .build_sync(&mut io)
        .expect("Failed to create editor");
```
- Initialize the board.
- Prepare input/output and buffers for the editor.
- Use `noline::EditorBuilder` to create a line-editing input prompt.

---

5. Game Loop
```rust
loop {
//TODO:
    //print board
    //get input
    //check if the input is valid (check if input is already occupied and is >=1 and <=9)
    //add the input to the board, check if any player has won, and switch players.
}
```
Task:
- Get user input from the editor.
- Validate and convert it to board coordinates.
- Check for wins or a full board.
- Switch players.

---

6. Displaying the Board

```rust
fn print_board(board: &[[char; 3]; 3]) {
    println!("\n--- Tic Tac Toe ---");
    for i in 0..3 {
        for j in 0..3 {
            let cell = board[i][j];
            let cell_num = i * 3 + j + 1;
            if cell == ' ' {
                print!(" {} ", cell_num);
            } else {
                print!(" {} ", cell);
            }
            if j < 2 {
                print!("|");
            }
        }
        println!();
        if i < 2 {
            println!("-----------");
        }
    }
    println!();
}
```

This visually shows the board with numbers for empty cells and symbols for occupied ones.

---

7. Helper Functions

Convert cell number to row/column:

```rust
fn cell_to_pos(cell: usize) -> (usize, usize) {
    let row = (cell - 1) / 3;
    let col = (cell - 1) % 3;
    (row, col)
}
```

---

Win condition checker:

```rust
fn check_winner(board: &[[char; 3]; 3], player: char) -> bool {
    //TODO: check all conditions for winning
}
```

---

Full board check:

```rust
fn board_full(board: &[[char; 3]; 3]) -> bool {
    board.iter().all(|row| row.iter().all(|&cell| cell != ' '))
}
```

---

## Build & Run

Rebuild the system and start QEMU:
```bash 
cargo start-qemu
```
To run it in Twizzler:
```
run tictactoe
got: <run tictactoe>
> --- Tic Tac Toe ---
 1 | 2 | 3
-----------
 4 | 5 | 6
-----------
 7 | 8 | 9

Player X, enter your move (1-9): 
```

Finally, once your code works, when you see twz> pop up, simply type tictactoe to play!
---

## Sample Output

```
--- Tic Tac Toe ---
 1 | 2 | 3
-----------
 4 | 5 | 6
-----------
 7 | 8 | 9

Player X, enter your move (1-9): 
```

## Extensions

You can enhance the game by:

- Using Twizzler to possibly persist the game.
- Supporting undo/redo using the input history buffer.

---
