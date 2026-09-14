//! Advisory work ownership, independent of LMDB's write transaction.
use std::{collections::HashSet, fs::File, io, sync::Mutex};

pub(crate) struct WorkLocks {
    file: File,
    held: Mutex<HashSet<u8>>,
}
pub(crate) struct WorkGuard<'a> {
    locks: &'a WorkLocks,
    stripe: u8,
}
impl WorkLocks {
    pub fn new(file: File) -> Self {
        Self {
            file,
            held: Mutex::new(HashSet::new()),
        }
    }

    pub fn acquire(&self, key: &[u8]) -> io::Result<Option<WorkGuard<'_>>> {
        let stripe = blake3::hash(key).as_bytes()[0];
        let mut held = self.held.lock().unwrap();
        if held.contains(&stripe) {
            return Ok(None);
        }
        if !self.set_lock(stripe, true)? {
            return Ok(None);
        }
        held.insert(stripe);
        Ok(Some(WorkGuard {
            locks: self,
            stripe,
        }))
    }

    #[cfg(target_os = "linux")]
    fn set_lock(&self, stripe: u8, locked: bool) -> io::Result<bool> {
        use std::os::fd::AsRawFd;
        // flock (writer admission) and OFD record locks (work stripes) are
        // independent on local Linux filesystems. Byte zero is reserved.
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = if locked { libc::F_WRLCK } else { libc::F_UNLCK } as _;
        lock.l_whence = libc::SEEK_SET as _;
        lock.l_start = i64::from(stripe) + 1;
        lock.l_len = 1;
        // One stable open description, with a local ownership set preventing
        // two threads from treating its merged locks as independent ownership.
        if unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_OFD_SETLK, &lock) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::EAGAIN | libc::EACCES)) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn set_lock(&self, _: u8, _: bool) -> io::Result<bool> {
        let _ = &self.file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "work ownership is not implemented on this platform",
        ))
    }
}
impl Drop for WorkGuard<'_> {
    fn drop(&mut self) {
        let mut held = self.locks.held.lock().unwrap();
        // If unlocking fails, retaining the local reservation is conservative;
        // callers eventually bypass waiting, and process exit releases the lock.
        if matches!(self.locks.set_lock(self.stripe, false), Ok(true)) {
            held.remove(&self.stripe);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn local_ownership_serializes_collisions_but_not_other_stripes() {
        let locks = WorkLocks::new(tempfile::tempfile().unwrap());
        let key = b"first request";
        let stripe = blake3::hash(key).as_bytes()[0];
        let collision = (0u32..)
            .map(u32::to_le_bytes)
            .find(|key| blake3::hash(key).as_bytes()[0] == stripe)
            .unwrap();
        let other = (0u32..)
            .map(u32::to_le_bytes)
            .find(|key| blake3::hash(key).as_bytes()[0] != stripe)
            .unwrap();
        let owner = locks.acquire(key).unwrap().unwrap();
        assert!(locks.acquire(key).unwrap().is_none());
        assert!(locks.acquire(&collision).unwrap().is_none());
        let independent = locks.acquire(&other).unwrap().unwrap();
        drop(owner);
        assert!(locks.acquire(&collision).unwrap().is_some());
        assert!(locks.acquire(&other).unwrap().is_none());
        drop(independent);
        assert!(locks.acquire(&other).unwrap().is_some());
    }
}
