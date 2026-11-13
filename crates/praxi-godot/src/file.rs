//! `Read` and `Seek` over Godot's `FileAccess`, so `res://` works in exported games
//! (`docs/features/godot.md#files`).

use godot::classes::FileAccess;
use godot::classes::file_access::ModeFlags;
use godot::prelude::*;
use std::io::{self, Read, Seek, SeekFrom};

pub struct GodotFile {
    file: Gd<FileAccess>,
}

impl GodotFile {
    pub fn open(path: &str) -> Result<GodotFile, String> {
        match FileAccess::open(path, ModeFlags::READ) {
            Some(file) => Ok(GodotFile { file }),
            None => Err(format!("cannot open {path}: {:?}", FileAccess::get_open_error())),
        }
    }
}

impl Read for GodotFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.file.get_length().saturating_sub(self.file.get_position());
        let n = (buf.len() as u64).min(left) as usize;
        if n == 0 {
            return Ok(0);
        }
        let data = self.file.get_buffer(n as i64);
        let got = data.as_slice();
        buf[..got.len()].copy_from_slice(got);
        Ok(got.len())
    }
}

impl Seek for GodotFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let to = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => self.file.get_position().checked_add_signed(d),
            SeekFrom::End(d) => self.file.get_length().checked_add_signed(d),
        };
        let to = to.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        self.file.seek(to);
        Ok(self.file.get_position())
    }
}
