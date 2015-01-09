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

extern crate libc;
extern crate termios;

use self::libc::{size_t, ssize_t, c_ushort, c_void};
use self::termios::{Termio, Termios};
use std::c_str::CString;
use std::io;
use std::io::fs::{AsFileDesc, fd_t, FileDesc};
use std::io::process::InheritFd;
use std::mem::transmute;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Relaxed};

mod raw {
    use std::io::fs::fd_t;
    use super::libc::{c_char, c_int, c_longlong, size_t, ssize_t, c_uint, c_void};

    // From x86_64-linux-gnu/bits/fcntl-linux.h
    #[cfg(target_arch="x86_64")]
    pub const SPLICE_F_NONBLOCK: c_uint = 2;

    // From asm-generic/ioctls.h
    pub const TIOCGWINSZ: c_int = 0x5413;

    // From asm-generic/posix_types.h
    #[allow(non_camel_case_types)]
    type loff_t = c_longlong;

    extern {
        pub fn ioctl(fd: c_int, req: c_int, ...) -> c_int;
        pub fn splice(fd_in: fd_t, off_in: *mut loff_t, fd_out: fd_t, off_out: *mut loff_t,
                      len: size_t, flags: c_uint) -> ssize_t;
    }

    #[link(name = "util")]
    extern {
        pub fn openpty(amaster: *mut fd_t, aslave: *mut fd_t, name: *mut c_char,
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

enum SpliceMode {
    Block,
    #[allow(dead_code)]
    NonBlock
}

fn splice(fd_in: &fd_t, fd_out: &fd_t, len: size_t, mode: SpliceMode) -> io::IoResult<ssize_t> {
    let flags = match mode {
        SpliceMode::Block => 0,
        SpliceMode::NonBlock => raw::SPLICE_F_NONBLOCK,
    };
    match unsafe { raw::splice(*fd_in, ptr::null_mut(), *fd_out, ptr::null_mut(), len, flags) } {
        -1 => Err(io::IoError::last_error()),
        s => Ok(s),
    }
}

fn get_winsize(fd: &FileDesc) -> io::IoResult<WinSize> {
    let mut ws = WinSize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    match unsafe { raw::ioctl(fd.fd(), raw::TIOCGWINSZ, &mut ws) } {
        0 => Ok(ws),
        _ => Err(io::standard_error(io::OtherIoError)),
    }
}

struct Pty {
    master: FileDesc,
    slave: FileDesc,
    path: Path,
}

pub struct TtyServer {
    master: FileDesc,
    slave: Option<FileDesc>,
    path: Path,
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
const MAX_PATH: uint = 4096;

unsafe fn opt2ptr<T>(e: &Option<&T>) -> *const c_void {
    match e {
        &Some(p) => transmute(p),
        &None => ptr::null(),
    }
}

// TODO: Return a StdStream (StdReader + StdWriter) or RtioTTY?
fn openpty(termp: Option<&Termios>, winp: Option<&WinSize>) -> io::IoResult<Pty> {
    let mut amaster: fd_t = -1;
    let mut aslave: fd_t = -1;
    let mut name = Vec::with_capacity(MAX_PATH);

    // TODO: Add a lock for future execve because close-on-exec
    match unsafe { raw::openpty(&mut amaster, &mut aslave, name.as_mut_ptr(),
            opt2ptr(&termp), opt2ptr(&winp)) } {
        0 => {
            let n = unsafe { CString::new(name.as_ptr(), false) };
            // TODO: Add signal handler for SIGWINCH
            Ok(Pty{
                master: FileDesc::new(amaster, true),
                slave: FileDesc::new(aslave, true),
                path: match n.as_str() {
                    Some(s) => match Path::new_opt(s) {
                        Some(p) => p,
                        None => return Err(io::standard_error(io::OtherIoError)),
                    },
                    None => return Err(io::standard_error(io::OtherIoError)),
                }
            })
        }
        _ => Err(io::IoError::last_error()),
    }
}

static SPLICE_BUFFER_SIZE: size_t = 1024;

fn splice_loop(do_flush: Arc<AtomicBool>, flush_event: Option<Sender<()>>, fd_in: fd_t, fd_out: fd_t) {
    'select: loop {
        if do_flush.load(Relaxed) {
            break 'select;
        }
        // FIXME: Add a select(2) watching for stdin and a pipe to stop the task
        // Need pipe to block on (the kernel only look at input)
        match splice(&fd_in, &fd_out, SPLICE_BUFFER_SIZE, SpliceMode::Block) {
            Ok(..) => {},
            Err(e) => {
                match e.kind {
                    // io::BrokenPipe
                    io::ResourceUnavailable => {},
                    _ => {
                        do_flush.store(true, Relaxed);
                        break 'select;
                    }
                }
            }
        }
    }
    match flush_event {
        Some(event) => {
            let _ = event.send_opt(());
        },
        None => {}
    }
}


impl TtyServer {
    /// Create a new TTY with the same configuration (termios and size) as the `template` TTY
    pub fn new(template: Option<&FileDesc>) -> io::IoResult<TtyServer> {
        // Native runtime does not support RtioTTY::get_winsize()
        let pty = match template {
            Some(t) => try!(openpty(Some(&try!(t.tcgetattr())), Some(&try!(get_winsize(t))))),
            None => try!(openpty(None, None)),
        };

        Ok(TtyServer {
            master: pty.master,
            slave: Some(pty.slave),
            path: pty.path,
        })
    }

    /// Bind the peer TTY with the server TTY
    pub fn new_client(&self, peer: FileDesc) -> io::IoResult<TtyClient> {
        let master = FileDesc::new(self.master.fd(), false);
        TtyClient::new(master, peer)
    }

    /// Get the server TTY path
    pub fn get_path(&self) -> &Path {
        &self.path
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
    pub fn spawn(&mut self, mut cmd: io::Command) -> io::IoResult<io::Process> {
        let mut drop_slave = false;
        let ret = match self.slave {
            Some(ref slave) => {
                drop_slave = true;
                let slave = InheritFd(slave.fd());
                // Force new session
                // TODO: tcsetpgrp
                cmd.stdin(slave).
                    stdout(slave).
                    stderr(slave).
                    detached().
                    spawn()
            },
            None => Err(io::standard_error(io::BrokenPipe))
        };
        if drop_slave {
            // Must close the slave file descriptor to not wait indefinitely the end of the proxy
            self.slave = None;
        }
        ret
    }
}

// TODO: Handle SIGWINCH to dynamically update WinSize
impl TtyClient {
    /// Setup the peer TTY client (e.g. stdio) and bind it to the master TTY server
    pub fn new(master: FileDesc, peer: FileDesc) -> io::IoResult<TtyClient> {
        // Setup peer terminal configuration
        let termios_orig = try!(peer.tcgetattr());
        let mut termios_peer = try!(peer.tcgetattr());
        termios_peer.local_flags.remove(termios::ECHO);
        termios_peer.local_flags.remove(termios::ICANON);
        termios_peer.local_flags.remove(termios::ISIG);
        termios_peer.input_flags.remove(termios::IGNBRK);
        termios_peer.input_flags.insert(termios::BRKINT);
        termios_peer.input_flags.remove(termios::ICRNL);
        termios_peer.control_chars[termios::ControlCharacter::VMIN as uint] = 1;
        termios_peer.control_chars[termios::ControlCharacter::VTIME as uint] = 0;
        // XXX: cfmakeraw
        try!(peer.tcsetattr(termios::When::TCSAFLUSH, &termios_peer));

        // Create the proxy
        let do_flush_main = Arc::new(AtomicBool::new(false));
        let (event_tx, event_rx): (Sender<()>, Receiver<()>) = channel();

        // Master to peer
        let (m2p_tx, m2p_rx) = match io::pipe::PipeStream::pair() {
            Ok(p) => (p.writer, p.reader),
            Err(e) => return Err(e),
        };
        let do_flush = do_flush_main.clone();
        let master_fd = master.fd();
        spawn(move || splice_loop(do_flush, None, master_fd, m2p_tx.as_fd().fd()));

        let do_flush = do_flush_main.clone();
        let peer_fd = peer.fd();
        spawn(move || splice_loop(do_flush, None, m2p_rx.as_fd().fd(), peer_fd));

        // Peer to master
        let (p2m_tx, p2m_rx) = match io::pipe::PipeStream::pair() {
            Ok(p) => (p.writer, p.reader),
            Err(e) => return Err(e),
        };
        let do_flush = do_flush_main.clone();
        let peer_fd = peer.fd();
        spawn(move || splice_loop(do_flush, None, peer_fd, p2m_tx.as_fd().fd()));

        let do_flush = do_flush_main.clone();
        let master_fd = master.fd();
        spawn(move || splice_loop(do_flush, Some(event_tx), p2m_rx.as_fd().fd(), master_fd));

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
            let _ = self.flush_event.recv_opt();
        }
    }
}

impl Drop for TtyClient {
    /// Cleanup the peer TTY
    fn drop(&mut self) {
        self.do_flush.store(true, Relaxed);
        let _ = self.peer.tcsetattr(termios::When::TCSAFLUSH, &self.termios_orig);
    }
}
