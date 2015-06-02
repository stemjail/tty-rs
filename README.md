# pty-rs

*pty* is a library to create and use a new pseudoterminal (PTY):
* `TtyServer`: create a PTY dedicated to a new command
* `TtyClient`: forward I/O from an existing TTY (user terminal)
* `FileDesc`: file descriptor wrapper

The I/O forward uses `splice(2)`, which is Linux specific, enabling zero-copy transfers.

For now, only the Rust 1.0.0-beta compiler can build this code because of the unfinished I/O reform.
Also note that this library uses *termios.rs* which is not available on *crates.io*.

This library is a work in progress.
The API may change.
