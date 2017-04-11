// Copyright (C) 2014-2015 Mickaël Salaün
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Lesser General Public License as published by
// the Free Software Foundation, version 3 of the License.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Lesser General Public License for more details.
//
// You should have received a copy of the GNU Lesser General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

#[macro_use]
extern crate chan;

extern crate chan_signal;
extern crate fd;
extern crate libc;
extern crate termios;

use chan_signal::Signal;
use fd::{Pipe, set_flags, splice_loop, unset_append_flag};
use ffi::{get_winsize, openpty, set_winsize};
use libc::c_int;
use std::fs::File;
use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use termios::{Termios, tcsetattr};

pub use fd::FileDesc;

pub mod ffi;

pub struct TtyServer {
    master: File,
    slave: Option<File>,
    path: PathBuf,
}

pub struct TtyClient {
    // Need to keep the master file descriptor open
    #[allow(dead_code)]
    master: FileDesc,
    master_status: Option<c_int>,
    peer: FileDesc,
    peer_status: Option<c_int>,
    termios_orig: Termios,
    do_flush: Arc<AtomicBool>,
    flush_event: Receiver<()>,
    // Automatically send an event when dropped
    _stop: chan::Sender<()>,
}

impl TtyServer {
    /// Create a new TTY with the same configuration (termios and size) as the `template` TTY
    pub fn new<T>(template: Option<&T>) -> io::Result<TtyServer> where T: AsRawFd {
        // Native runtime does not support RtioTTY::get_winsize()
        let pty = match template {
            Some(t) => try!(openpty(Some(&try!(Termios::from_fd(t.as_raw_fd()))), Some(&try!(get_winsize(t))))),
            None => try!(openpty(None, None)),
        };

        Ok(TtyServer {
            master: pty.master,
            slave: Some(pty.slave),
            path: pty.path,
        })
    }

    /// Bind the peer TTY with the server TTY
    ///
    /// The sigwinch_handler must handle the SIGWINCH signal to update the TTY window size.
    /// This handler can be created with `chan_signal::notify(&[Signal::WINCH])` from the
    /// chan_signal crate.
    ///
    /// Any and all threads spawned must come after the first call to chan_signal::notify!
    pub fn new_client<T>(&self, peer: T, sigwinch_handler: Option<chan::Receiver<Signal>>) ->
            io::Result<TtyClient> where T: AsRawFd + IntoRawFd {
        let master = FileDesc::new(self.master.as_raw_fd(), false);
        TtyClient::new(master, peer, sigwinch_handler)
    }

    /// Get the TTY master file descriptor usable by a `TtyClient`
    pub fn get_master(&self) -> &File {
        &self.master
    }

    /// Take the TTY slave file descriptor to manually pass it to a process
    pub fn take_slave(&mut self) -> Option<File> {
        self.slave.take()
    }

    /// Spawn a new process connected to the slave TTY
    pub fn spawn(&mut self, mut cmd: Command) -> io::Result<Child> {

        let slave = match self.slave.take() {
            Some(slave) => slave,
            None => return Err(io::Error::new(io::ErrorKind::BrokenPipe, "No TTY slave")),
        };

        let new_slave = FileDesc::new(slave.as_raw_fd(), false);
        let stdin_fd = new_slave.dup().unwrap();
        let stdout_fd = new_slave.dup().unwrap();
        let stderr_fd = new_slave.dup().unwrap();

        let child = cmd.stdin(unsafe { Stdio::from_raw_fd(stdout_fd.into_raw_fd()) }).
                        stdout(unsafe { Stdio::from_raw_fd(stdin_fd.into_raw_fd()) }).
                        // Must close the slave FD to not wait indefinitely the end of the proxy
                        stderr(unsafe { Stdio::from_raw_fd(stderr_fd.into_raw_fd()) }).
                        // Don't check the error of setsid because it fails if we're the
                        // process leader already. We just forked so it shouldn't return
                        // error, but ignore it anyway.
                        before_exec(|| { let _ = unsafe { libc::setsid() }; Ok(()) }).
                        spawn();

        child
    }
}

impl AsRef<Path> for TtyServer {
    /// Get the server TTY path
    fn as_ref(&self) -> &Path {
        self.path.as_ref()
    }
}

// Ignore errors
fn copy_winsize<T, U>(src: &T, dst: &U) where T: AsRawFd, U: AsRawFd {
    if let Ok(ws) = get_winsize(src) {
        let _ = set_winsize(dst, &ws);
    }
}

// TODO: Handle SIGWINCH to dynamically update WinSize
// TODO: Replace `spawn` with `scoped` and share variables
impl TtyClient {
    /// Setup the peer TTY client (e.g. stdio) and bind it to the master TTY server
    ///
    /// The sigwinch_handler must handle the SIGWINCH signal to update the TTY window size.
    /// This handler can be created with `chan_signal::notify(&[Signal::WINCH])` from the
    /// chan_signal crate.
    ///
    /// Any and all threads spawned must come after the first call to chan_signal::notify!
    pub fn new<T, U>(master: T, peer: U, sigwinch_handler: Option<chan::Receiver<Signal>>) ->
            io::Result<TtyClient> where T: AsRawFd + IntoRawFd, U: AsRawFd + IntoRawFd {
        // Setup peer terminal configuration
        let termios_orig = try!(Termios::from_fd(peer.as_raw_fd()));
        let mut termios_peer = try!(Termios::from_fd(peer.as_raw_fd()));
        termios_peer.c_lflag &= !(termios::ECHO | termios::ICANON | termios::ISIG);
        termios_peer.c_iflag &= !(termios::IGNBRK | termios::ICRNL);
        termios_peer.c_iflag |= termios::BRKINT;
        termios_peer.c_cc[termios::VMIN] = 1;
        termios_peer.c_cc[termios::VTIME] = 0;
        // XXX: cfmakeraw
        try!(tcsetattr(peer.as_raw_fd(), termios::TCSAFLUSH, &termios_peer));

        // Create the proxy
        let do_flush_main = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx): (Sender<()>, Receiver<()>) = channel();

        // Master to peer
        let (m2p_tx, m2p_rx) = match Pipe::new() {
            Ok(p) => (p.writer, p.reader),
            Err(e) => return Err(io::Error::new(io::ErrorKind::Other, e)),
        };
        let do_flush = do_flush_main.clone();
        let master_fd = master.as_raw_fd();
        thread::spawn(move || splice_loop(do_flush, None, master_fd, m2p_tx.as_raw_fd()));

        let do_flush = do_flush_main.clone();
        let peer_fd = peer.as_raw_fd();
        let peer_status = try!(unset_append_flag(peer_fd));
        thread::spawn(move || splice_loop(do_flush, None, m2p_rx.as_raw_fd(), peer_fd));

        // Peer to master
        let (p2m_tx, p2m_rx) = match Pipe::new() {
            Ok(p) => (p.writer, p.reader),
            Err(e) => return Err(io::Error::new(io::ErrorKind::Other, e)),
        };
        let do_flush = do_flush_main.clone();
        let peer_fd = peer.as_raw_fd();
        thread::spawn(move || splice_loop(do_flush, None, peer_fd, p2m_tx.as_raw_fd()));

        let do_flush = do_flush_main.clone();
        let master_fd = master.as_raw_fd();
        let master_status = try!(unset_append_flag(master_fd));
        thread::spawn(move || splice_loop(do_flush, Some(event_tx), p2m_rx.as_raw_fd(), master_fd));

        // Handle terminal resizing
        let (stop_tx, stop_rx) = chan::sync(0);
        if let Some(signal) = sigwinch_handler {
            // master and peer FD will be close by TtyClient::drop()
            let master2 = FileDesc::new(master.as_raw_fd(), false);
            let peer2 = FileDesc::new(peer.as_raw_fd(), false);
            thread::spawn(move || {
                'select: loop {
                    chan_select! {
                        signal.recv() -> signal => {
                            if signal != Some(Signal::WINCH) {
                                continue 'select;
                            }
                            copy_winsize(&peer2, &master2);
                        },
                        stop_rx.recv() => {
                            break;
                        }
                    }
                }
            });
        }

        Ok(TtyClient {
            master: FileDesc::new(master.into_raw_fd(), true),
            master_status: master_status,
            peer: FileDesc::new(peer.into_raw_fd(), true),
            peer_status: peer_status,
            termios_orig: termios_orig,
            do_flush: do_flush_main,
            flush_event: event_rx,
            _stop: stop_tx,
        })
    }

    /// Wait until the TTY binding broke (e.g. the connected process exited)
    pub fn wait(&self) {
        while !self.do_flush.load(Relaxed) {
            let _ = self.flush_event.recv();
        }
    }

    /// Update the terminal window size according to the peer
    pub fn update_winsize(&mut self) {
        copy_winsize(&self.peer, &self.master);
    }
}

impl Drop for TtyClient {
    /// Cleanup the peer TTY
    fn drop(&mut self) {
        self.do_flush.store(true, Relaxed);
        let _ = tcsetattr(self.peer.as_raw_fd(), termios::TCSAFLUSH, &self.termios_orig);

        // Restore the append flag if needed
        let tty_fd = [(&self.peer, self.peer_status), (&self.master, self.master_status)];
        for &(fd, status) in tty_fd.iter() {
            if let Some(s) = status {
                let _ = set_flags(fd.as_raw_fd(), s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_openpty_race(cmd: &str) -> io::Result<Child> {

        let mut tty = TtyServer::new::<FileDesc>(None).unwrap();
        let mut cmd = Command::new(&cmd);
        cmd.env_clear();

        tty.spawn(cmd)
    }

    #[test]
    fn openpty_race_true() {
        for _ in 1..1000 {
            let mut process = create_openpty_race("/bin/true").unwrap();
            assert_eq!(process.wait().unwrap().code().unwrap(), 0);
        }
    }

    #[test]
    fn openpty_race_false() {
        for _ in 1..1000 {
            let mut process = create_openpty_race("/bin/false").unwrap();
            assert_ne!(process.wait().unwrap().code().unwrap(), 0);
        }
    }
}
