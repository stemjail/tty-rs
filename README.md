# pty-rs

*pty* is a library to create and use a new pseudoterminal (PTY):
* `TtyServer`: create a PTY dedicated to a new command
* `TtyClient`: forward I/O from an existing TTY (user terminal)
* `FileDesc`: file descriptor wrapper

The I/O forward uses `splice(2)`, which is Linux specific, enabling zero-copy transfers.

You need to use Rust 1.3.0-dev (nightly) to build this crate.

This library is a work in progress.
The API may change.
