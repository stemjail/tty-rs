# pty-rs

*pty* is a library to create and use a new pseudoterminal (PTY):
* `TtyServer`: create a new PTY (with a `TtyClient`) and send TTY data to the master side
* `TtyClient`: spawn a new command and give it a TTY slave side
* `FileDesc`: file descriptor wrapper (closed when dropped)

The data transfert between the client and server sides is a zero-copy, thanks to `splice(2)` (Linux specific).

This library is a work in progress.
The API may change.
