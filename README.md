# tty-rs

*tty* is a thread-safe library to create and use a new pseudoterminal (PTY):
* `TtyServer`: create a PTY dedicated to a new command
* `TtyClient`: forward I/O from an existing TTY (user terminal)

The I/O forward uses `splice(2)`, which is Linux specific, enabling zero-copy transfers.

Build with Rust >= 1.15.0 .

This library is a work in progress.
The API may change.
