//! A file the editor can close while its player still holds it, so that "Write cues into .praxi"
//! can rename over it on Windows (`docs/features/cue-editor.md#saving`).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct SharedFile(Arc<Mutex<Option<File>>>);

impl SharedFile {
    pub fn open(path: &Path) -> io::Result<SharedFile> {
        Ok(SharedFile(Arc::new(Mutex::new(Some(File::open(path)?)))))
    }

    /// Closes the file; reads and seeks fail from now on.
    pub fn close(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    fn with<T>(&self, f: impl FnOnce(&mut File) -> io::Result<T>) -> io::Result<T> {
        match self.0.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            Some(file) => f(file),
            None => Err(io::Error::other("the file is closed")),
        }
    }
}

impl Read for SharedFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.with(|f| f.read(buf))
    }
}

impl Seek for SharedFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.with(|f| f.seek(pos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_releases_the_file() {
        let dir = std::env::temp_dir().join(format!("praxi-editor-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (path, other) = (dir.join("a"), dir.join("b"));
        std::fs::write(&path, b"old").unwrap();
        std::fs::write(&other, b"new").unwrap();
        let file = SharedFile::open(&path).unwrap();
        let mut reader = file.clone();
        let mut buf = [0; 2];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ol");
        file.close();
        assert!(reader.read(&mut buf).is_err());
        assert!(reader.seek(SeekFrom::Start(0)).is_err());
        std::fs::rename(&other, &path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
