// Copyright (C) 2014 Mickaël Salaün
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

#![feature(libc)]
#![feature(process_session_leader)]

extern crate fd;
extern crate libc;
extern crate termios;

use fd::{Pipe, splice_loop};
use libc::{c_char, c_ushort, c_void, strlen};
use std::ffi::CString;
use std::io;
use std::mem::transmute;
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::io::FromRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use termios::{Termios, tcsetattr};

pub use fd::FileDesc;

mod raw {
    use libc::{c_char, c_int, c_void};
    use std::os::unix::io::RawFd;

    // From asm-generic/ioctls.h
    pub const TIOCGWINSZ: c_int = 0x5413;
    pub const FIOCLEX: c_int = 0x5451;

    extern {
        pub fn ioctl(fd: c_int, req: c_int, ...) -> c_int;
    }

    #[link(name = "util")]
    extern {
        pub fn openpty(amaster: *mut RawFd, aslave: *mut RawFd, name: *mut c_char,
                       termp: *const c_void, winp: *const c_void) -> c_int;
    }
}

// From termios.h
#[repr(C)]
struct WinSize {
    ws_row: c_ushort,
    ws_col: c_ushort,
    ws_xpixel: c_ushort,
    ws_ypixel: c_ushort,
}

fn get_winsize(fd: &AsRawFd) -> io::Result<WinSize> {
    let mut ws = WinSize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    match unsafe { raw::ioctl(fd.as_raw_fd(), raw::TIOCGWINSZ, &mut ws) } {
        0 => Ok(ws),
        _ => Err(io::Error::last_os_error()),
    }
}

struct Pty {
    master: FileDesc,
    slave: FileDesc,
    path: PathBuf,
}

pub struct TtyServer {
    master: FileDesc,
    slave: Option<FileDesc>,
    path: PathBuf,
}

pub struct TtyClient {
    // Need to keep the master file descriptor open
    #[allow(dead_code)]
    master: FileDesc,
    peer: FileDesc,
    termios_orig: Termios,
    do_flush: Arc<AtomicBool>,
    flush_event: Receiver<()>,
}

// From linux/limits.h
const MAX_PATH: usize = 4096;

unsafe fn opt2ptr<T>(e: &Option<&T>) -> *const c_void {
    match e {
        &Some(p) => transmute(p),
        &None => ptr::null(),
    }
}

// TODO: Return a StdStream (StdReader + StdWriter) or RtioTTY?
fn openpty(termp: Option<&Termios>, winp: Option<&WinSize>) -> io::Result<Pty> {
    let mut amaster: RawFd = -1;
    let mut aslave: RawFd = -1;
    let mut name = Vec::with_capacity(MAX_PATH);

    // TODO: Add a lock for future execve because close-on-exec
    match unsafe { raw::openpty(&mut amaster, &mut aslave, name.as_mut_ptr() as *mut libc::c_char,
            opt2ptr(&termp), opt2ptr(&winp)) } {
        0 => {
            unsafe {
                // TODO: Fix thread-safe
                let _ = raw::ioctl(amaster, raw::FIOCLEX);
                let _ = raw::ioctl(aslave, raw::FIOCLEX);

                // FFI string hack because of the foolish openpty(3) API!
                let ptr = name.as_ptr() as *const c_char;
                // Don't lie to Rust about the buffer length from strlen(3)
                name.set_len(1 +  strlen(ptr) as usize);
                // Cleanly remove the trailing 0 for CString
                let _ = name.pop();
            }
            let n = try!(CString::new(name));
            let n = match std::str::from_utf8(n.to_bytes()) {
                Ok(n) => n,
                Err(e) => return Err(io::Error::new(io::ErrorKind::Other, e)),
            };
            // TODO: Add signal handler for SIGWINCH
            Ok(Pty{
                master: FileDesc::new(amaster, true),
                slave: FileDesc::new(aslave, true),
                path: PathBuf::from(n),
            })
        }
        _ => Err(io::Error::last_os_error()),
    }
}


// TODO: Replace most &FileDesc with AsRawFd
impl TtyServer {
    /// Create a new TTY with the same configuration (termios and size) as the `template` TTY
    pub fn new(template: Option<&FileDesc>) -> io::Result<TtyServer> {
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
    pub fn new_client(&self, peer: FileDesc) -> io::Result<TtyClient> {
        let master = FileDesc::new(self.master.as_raw_fd(), false);
        TtyClient::new(master, peer)
    }

    /// Get the TTY master file descriptor usable by a `TtyClient`
    pub fn get_master(&self) -> &FileDesc {
        &self.master
    }

    /// Take the TTY slave file descriptor to manually pass it to a process
    pub fn take_slave(&mut self) -> Option<FileDesc> {
        self.slave.take()
    }

    /// Spawn a new process connected to the slave TTY
    pub fn spawn(&mut self, mut cmd: Command) -> io::Result<Child> {
        match self.slave.take() {
            Some(slave) => {
                // Force new session
                // TODO: tcsetpgrp
                cmd.stdin(unsafe { Stdio::from_raw_fd(slave.as_raw_fd()) }).
                    stdout(unsafe { Stdio::from_raw_fd(slave.as_raw_fd()) }).
                    // Must close the slave FD to not wait indefinitely the end of the proxy
                    stderr(unsafe { Stdio::from_raw_fd(slave.into()) }).
                    session_leader(true).
                    spawn()
            },
            None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "No TTY slave")),
        }
    }
}

impl AsRef<Path> for TtyServer {
    /// Get the server TTY path
    fn as_ref(&self) -> &Path {
        self.path.as_ref()
    }
}

// TODO: Handle SIGWINCH to dynamically update WinSize
// TODO: Replace `spawn` with `scoped` and share variables
impl TtyClient {
    /// Setup the peer TTY client (e.g. stdio) and bind it to the master TTY server
    pub fn new(master: FileDesc, peer: FileDesc) -> io::Result<TtyClient> {
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
        thread::spawn(move || splice_loop(do_flush, Some(event_tx), p2m_rx.as_raw_fd(), master_fd));

        Ok(TtyClient {
            master: master,
            peer: peer,
            termios_orig: termios_orig,
            do_flush: do_flush_main,
            flush_event: event_rx,
        })
    }

    /// Wait until the TTY binding broke (e.g. the connected process exited)
    pub fn wait(&self) {
        while !self.do_flush.load(Relaxed) {
            let _ = self.flush_event.recv();
        }
    }
}

impl Drop for TtyClient {
    /// Cleanup the peer TTY
    fn drop(&mut self) {
        self.do_flush.store(true, Relaxed);
        let _ = tcsetattr(self.peer.as_raw_fd(), termios::TCSAFLUSH, &self.termios_orig);
    }
}
