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

use libc::{self, c_char, c_int, c_uint, c_ushort};
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::path::{Path, PathBuf};
use termios::{self, Termios, tcsetattr};

const DEV_PTMX_PATH: &'static str = "/dev/ptmx";
const DEV_PTS_PATH: &'static str = "/dev/pts";

mod raw {
    use libc::{c_int, c_uint};

    // From asm-generic/fcntl.h
    pub const O_CLOEXEC: c_int = 0o2000000;

    // From asm-generic/ioctls.h
    pub const TIOCGWINSZ: c_int = 0x5413;
    pub const TIOCSWINSZ: c_int = 0x5414;
    pub const TIOCGPTN: c_uint = 0x80045430;

    extern {
        pub fn grantpt(fd: c_int) -> c_int;
        pub fn ioctl(fd: c_int, req: c_int, ...) -> c_int;
        pub fn unlockpt(fd: c_int) -> c_int;
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

pub fn get_winsize<T>(slave: &T) -> io::Result<WinSize> where T: AsRawFd {
    let mut ws = WinSize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    match unsafe { raw::ioctl(slave.as_raw_fd(), raw::TIOCGWINSZ, &mut ws) } {
        0 => Ok(ws),
        _ => Err(io::Error::last_os_error()),
    }
}

pub fn set_winsize<T>(slave: &T, ws: &WinSize) -> io::Result<()> where T: AsRawFd {
    match unsafe { raw::ioctl(slave.as_raw_fd(), raw::TIOCSWINSZ, ws) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

pub struct Pty {
    pub master: File,
    pub slave: File,
    pub path: PathBuf,
}

fn open_noctty<T>(path: &T) -> io::Result<File> where T: AsRef<Path> {
    let flags = raw::O_CLOEXEC | libc::O_NOCTTY | libc::O_RDWR;
    // The CString unwrap always succeed on unix
    let cstr = CString::new(path.as_ref().as_os_str().as_bytes()).unwrap();
    match unsafe { libc::open(cstr.as_ptr(), flags, 0) } {
        -1 => Err(io::Error::last_os_error()),
        fd => Ok(unsafe { File::from_raw_fd(fd) }),
    }
}

// Need our own `getpt()` to be able to open with O_CLOEXEC
#[cfg(target_os = "linux")]
pub fn getpt() -> io::Result<File> {
    open_noctty(&DEV_PTMX_PATH)
}

pub fn grantpt<T>(master: &mut T) -> io::Result<()> where T: AsRawFd {
    match unsafe { raw::grantpt(master.as_raw_fd()) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

pub fn unlockpt<T>(master: &mut T) -> io::Result<()> where T: AsRawFd {
    match unsafe { raw::unlockpt(master.as_raw_fd()) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

pub fn ptsindex<T>(master: &mut T) -> io::Result<u32> where T: AsRawFd {
    let mut idx: c_uint = 0;
    match unsafe { raw::ioctl(master.as_raw_fd(), raw::TIOCGPTN as c_int, &mut idx) } {
        0 => Ok(idx),
        _ => Err(io::Error::last_os_error()),
    }
}

pub fn ptsname<T>(master: &mut T) -> io::Result<PathBuf> where T: AsRawFd {
    Ok(Path::new(DEV_PTS_PATH).join(format!("{}", try!(ptsindex(master)))))
}

/// Thread-safe (i.e. reentrant) version of `openpty(3)`
pub fn openpty(termp: Option<&Termios>, winp: Option<&WinSize>) -> io::Result<Pty> {
    let mut master = try!(getpt());
    try!(grantpt(&mut master));
    try!(unlockpt(&mut master));
    let name = try!(ptsname(&mut master));
    let slave = try!(open_noctty(&name));

    match termp {
        Some(t) => try!(tcsetattr(slave.as_raw_fd(), termios::TCSAFLUSH, &t)),
        None => {}
    }
    match winp {
        Some(w) => try!(set_winsize(&slave, w)),
        None => {}
    }

    // TODO: Add signal handler for SIGWINCH
    Ok(Pty{
        master: master,
        slave: slave,
        path: name,
    })
}
