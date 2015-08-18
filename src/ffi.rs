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

use fd::FileDesc;
use libc::{c_char, c_ushort, c_void, strlen};
use std::ffi::CString;
use std::io;
use std::mem::transmute;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::ptr;
use termios::Termios;

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
pub struct WinSize {
    ws_row: c_ushort,
    ws_col: c_ushort,
    ws_xpixel: c_ushort,
    ws_ypixel: c_ushort,
}

pub fn get_winsize(fd: &AsRawFd) -> io::Result<WinSize> {
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

pub struct Pty {
    pub master: FileDesc,
    pub slave: FileDesc,
    pub path: PathBuf,
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
pub fn openpty(termp: Option<&Termios>, winp: Option<&WinSize>) -> io::Result<Pty> {
    let mut amaster: RawFd = -1;
    let mut aslave: RawFd = -1;
    let mut name = Vec::with_capacity(MAX_PATH);

    // TODO: Add a lock for future execve because close-on-exec
    match unsafe { raw::openpty(&mut amaster, &mut aslave, name.as_mut_ptr() as *mut c_char,
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
            let n = match ::std::str::from_utf8(n.to_bytes()) {
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

