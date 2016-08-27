# tty-rs

*tty* is a thread-safe library to create and use a new pseudoterminal (PTY):
* `TtyServer`: create a PTY dedicated to a new command
* `TtyClient`: forward I/O from an existing TTY (user terminal)

The I/O forward uses `splice(2)`, which is Linux specific, enabling zero-copy transfers.

You need to use a nightly Rust channel >= 1.8.0-dev to build this crate (because of unstable API use).

This library is a work in progress.
The API may change.

This library does not yet support signal handling (e.g. terminal resize, Ctrl-C).
