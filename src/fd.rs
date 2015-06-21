// Copyright (C) 2015 Mickaël Salaün
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

use std::fs::File;
use libc::c_int;
use std::io;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};

#[derive(Debug)]
#[cfg(unix)]
pub struct FileDesc {
    fd: RawFd,
    close_on_drop: bool,
}

impl FileDesc {
    pub fn new(fd: RawFd, close_on_drop: bool) -> FileDesc {
        FileDesc {
            fd: fd,
            close_on_drop: close_on_drop,
        }
    }

    pub fn dup(&self) -> io::Result<FileDesc> {
        Ok(FileDesc {
            fd: match unsafe { ::libc::dup(self.fd) } {
                -1 => return Err(io::Error::last_os_error()),
                n => n,
            },
            close_on_drop: self.close_on_drop,
        })
    }
}

impl Drop for FileDesc {
    fn drop(&mut self) {
        if self.close_on_drop {
            unsafe { ::libc::close(self.fd); }
        }
    }
}

impl AsRawFd for FileDesc {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Into<RawFd> for FileDesc {
    fn into(mut self) -> RawFd {
        self.close_on_drop = false;
        self.fd
    }
}

/// A pipe(2) interface
pub struct Pipe {
    pub reader: File,
    pub writer: File,
}

impl Pipe {
    pub fn new() -> io::Result<Pipe> {
        let mut fds: (c_int, c_int) = (-1, -1);
        let fdp: *mut c_int = unsafe { ::std::mem::transmute(&mut fds) };
        // TODO: Use pipe2(2) with O_CLOEXEC
        if unsafe { ::libc::pipe(fdp) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Pipe {
            reader: unsafe { File::from_raw_fd(fds.0) },
            writer: unsafe { File::from_raw_fd(fds.1) },
        })
    }
}
