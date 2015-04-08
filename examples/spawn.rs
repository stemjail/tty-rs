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

#![feature(io)]
#![feature(libc)]
#![feature(path)]

extern crate libc;
extern crate pty;

use pty::FileDesc;
use pty::TtyServer;
use std::old_io as io;

fn main() {
    let stdin = FileDesc::new(libc::STDIN_FILENO, false);
    let mut server = match TtyServer::new(Some(&stdin)) {
        Ok(s) => s,
        Err(e) => panic!("Error TTY server: {}", e),
    };
    println!("Got PTY {}", server.get_path().display());
    let proxy = match server.new_client(stdin) {
        Ok(p) => p,
        Err(e) => panic!("Error TTY client: {}", e),
    };

    // Should call setsid -c sh
    let cmd = io::Command::new(Path::new("/bin/sh"));
    let process = match server.spawn(cmd) {
        Ok(p) => p,
        Err(e) => panic!("Failed to execute process: {}", e),
    };
    println!("spawned {}", process.id());
    proxy.wait();
    println!("quit");
}
