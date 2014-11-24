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
    name: String,
}

pub struct TtyServer {
    pty: Pty,
}

pub struct TtyClient {
    master: FileDesc,
    peer: FileDesc,
    termios_orig: Termios,
    do_flush: Arc<AtomicBool>,
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
                name: match n.as_str() {
                    Some(s) => s.to_string(),
                    None => return Err(io::standard_error(io::OtherIoError)),
                }
            })
        }
        _ => Err(io::IoError::last_error()),
    }
}

static SPLICE_BUFFER_SIZE: size_t = 1024;

fn splice_loop(do_flush: Arc<AtomicBool>, fd_in: fd_t, fd_out: fd_t) {
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
}


impl TtyServer {
    pub fn new(template: &FileDesc) -> io::IoResult<TtyServer> {
        let termios = try!(template.tcgetattr());
        // TODO: Handle SIGWINCH to dynamically update WinSize
        // Native runtime does not support RtioTTY::get_winsize()
        let size = try!(get_winsize(template));
        let pty = try!(openpty(Some(&termios), Some(&size)));

        Ok(TtyServer {
            pty: pty,
        })
    }

    pub fn new_client(&self, stdio: FileDesc) -> io::IoResult<TtyClient> {
        let master = FileDesc::new(self.pty.master.fd(), false);
        TtyClient::new(master, stdio)
    }

    pub fn get_master(&self) -> &FileDesc {
        &self.pty.master
    }

    pub fn get_name(&self) -> &String {
        &self.pty.name
    }

    pub fn spawn(&self, mut cmd: io::Command) -> io::IoResult<io::Process> {
        let slave = InheritFd(self.pty.slave.fd());
        // Force new session
        // TODO: tcsetpgrp
        cmd.stdin(slave).
            stdout(slave).
            stderr(slave).
            detached().
            spawn()
    }
}

impl TtyClient {
    pub fn new(master: FileDesc, stdio: FileDesc) -> io::IoResult<TtyClient> {
        // Setup peer terminal configuration
        let termios_orig = try!(stdio.tcgetattr());
        let mut termios_peer = try!(stdio.tcgetattr());
        termios_peer.local_flags.remove(termios::ECHO);
        termios_peer.local_flags.remove(termios::ICANON);
        termios_peer.local_flags.remove(termios::ISIG);
        termios_peer.input_flags.remove(termios::IGNBRK);
        termios_peer.input_flags.insert(termios::BRKINT);
        termios_peer.input_flags.remove(termios::ICRNL);
        termios_peer.control_chars[termios::ControlCharacter::VMIN as uint] = 1;
        termios_peer.control_chars[termios::ControlCharacter::VTIME as uint] = 0;
        // XXX: cfmakeraw
        try!(stdio.tcsetattr(termios::When::TCSAFLUSH, &termios_peer));

        let do_flush = Arc::new(AtomicBool::new(false));
        let tty = TtyClient {
            master: master,
            peer: stdio,
            termios_orig: termios_orig,
            do_flush: do_flush,
        };
        try!(tty.create_proxy());
        Ok(tty)
    }

    fn create_proxy(&self) -> io::IoResult<()> {
        // Master to peer
        let (m2p_tx, m2p_rx) = match io::pipe::PipeStream::pair() {
            Ok(p) => (p.writer, p.reader),
            Err(e) => return Err(e),
        };
        let do_flush = self.do_flush.clone();
        let master_fd = self.master.fd();
        spawn(proc() splice_loop(do_flush, master_fd, m2p_tx.as_fd().fd()));

        let do_flush = self.do_flush.clone();
        let peer_fd = self.peer.fd();
        spawn(proc() splice_loop(do_flush, m2p_rx.as_fd().fd(), peer_fd));

        // Peer to master
        let (p2m_tx, p2m_rx) = match io::pipe::PipeStream::pair() {
            Ok(p) => (p.writer, p.reader),
            Err(e) => return Err(e),
        };
        let do_flush = self.do_flush.clone();
        let peer_fd = self.peer.fd();
        spawn(proc() splice_loop(do_flush, peer_fd, p2m_tx.as_fd().fd()));

        let do_flush = self.do_flush.clone();
        let master_fd = self.master.fd();
        spawn(proc() splice_loop(do_flush, p2m_rx.as_fd().fd(), master_fd));

        Ok(())
    }
}

impl Drop for TtyClient {
    fn drop(&mut self) {
        self.do_flush.store(true, Relaxed);
        let _ = self.peer.tcsetattr(termios::When::TCSAFLUSH, &self.termios_orig);
    }
}
