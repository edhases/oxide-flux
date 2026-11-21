//! Byte-addressed block I/O and the file set a torrent is stored in.
//!
//! [`BlockBackend`] is the seam a future `io_uring` / IOCP backend plugs into: it
//! never sees torrent semantics, only "open this path", "read/write this many bytes
//! at this offset", "preallocate", "sync". [`StdBackend`] is the portable
//! `std::fs` implementation that is shipped today.
//!
//! No `unsafe` is used anywhere: positional I/O goes through the safe
//! `FileExt` APIs on Unix/Windows.

use std::any::Any;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::DiskError;
use crate::layout::FileLayout;

/// Number of zero bytes written per call by [`BlockBackend::allocate_and_zero`].
const ZERO_CHUNK: usize = 1 << 20;

/// How a [`FileSet`] prepares files when it opens them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Allocation {
    /// Leave whatever is on disk in place; create missing files with length 0.
    #[default]
    None,
    /// Grow each file to its declared length with `set_len`. The result is sparse on
    /// every platform we support, so it is fast and does not touch the data.
    Sparse,
    /// Grow and explicitly zero each file, so the filesystem really commits the
    /// blocks (useful before handing a file to a player that mmaps it).
    Zeroed,
}

/// Private payload wrapper. It exists so that `ErasedHandle` is *not* implemented for
/// every `T: Any` — if it were, `Arc<dyn ErasedHandle>` would implement it too and
/// method resolution would happily downcast the `Arc` instead of the payload.
struct HandleBox<T: Any + Send + Sync>(T);

trait ErasedHandle: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

impl<T: Any + Send + Sync> ErasedHandle for HandleBox<T> {
    fn as_any(&self) -> &dyn Any {
        &self.0
    }
}

/// Opaque handle returned by [`BlockBackend::open`].
///
/// The disk layer never inspects it; it is just passed back to the backend that
/// created it. A native backend can store its registered buffer/file id here
/// without changing this crate's surface.
#[derive(Clone)]
pub struct FileHandle {
    inner: Arc<dyn ErasedHandle>,
}

impl FileHandle {
    /// Wraps a backend-specific payload. `StdBackend` wraps a `std::fs::File`.
    pub fn new<T: Any + Send + Sync>(payload: T) -> Self {
        Self {
            inner: Arc::new(HandleBox(payload)),
        }
    }

    /// Borrows the payload if it has type `T`.
    pub fn get<T: Any>(&self) -> Option<&T> {
        self.inner.as_any().downcast_ref::<T>()
    }
}

impl fmt::Debug for FileHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("FileHandle(<opaque>)")
    }
}

/// Block-addressed storage device.
///
/// All methods are synchronous and are only ever called from the dedicated writer
/// and verifier threads, never from the async runtime. Implementations must be
/// safe to call from several threads at once (the verifier runs a pool).
pub trait BlockBackend: Send + Sync + 'static {
    /// Opens `path` for reading and writing, creating it when absent.
    fn open(&self, path: &Path) -> io::Result<FileHandle>;

    /// Prepares `len` bytes. Backends may treat this as advisory; `StdBackend`
    /// only acts on it when constructed with `preallocate = true`, using a sparse
    /// `set_len`.
    fn preallocate(&self, handle: &FileHandle, len: u64) -> io::Result<()>;

    /// Prepares `len` bytes and guarantees they are readable as zeroes without
    /// further I/O. The default implementation falls back to [`Self::preallocate`].
    fn allocate_and_zero(&self, handle: &FileHandle, len: u64) -> io::Result<()> {
        self.preallocate(handle, len)
    }

    /// Reads exactly `buf.len()` bytes starting at `offset`.
    fn read_exact_at(&self, handle: &FileHandle, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    /// Writes all of `buf` starting at `offset`.
    fn write_all_at(&self, handle: &FileHandle, offset: u64, buf: &[u8]) -> io::Result<()>;

    /// Flushes a single file's data to stable storage.
    fn sync(&self, handle: &FileHandle) -> io::Result<()>;
}

/// Portable `std::fs` backend.
///
/// `preallocate` toggles the sparse `set_len` used by [`BlockBackend::preallocate`];
/// it does not affect [`BlockBackend::allocate_and_zero`], which always really
/// zeroes the range because the caller asked for it explicitly.
#[derive(Debug, Clone, Copy)]
pub struct StdBackend {
    preallocate: bool,
}

impl StdBackend {
    /// Creates a backend. Pass `true` to make [`BlockBackend::preallocate`] grow
    /// files to their declared length with `set_len`.
    pub const fn new(preallocate: bool) -> Self {
        Self { preallocate }
    }

    fn file<'a>(&self, handle: &'a FileHandle) -> io::Result<&'a File> {
        handle.get::<File>().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "file handle was not created by StdBackend",
            )
        })
    }
}

impl Default for StdBackend {
    fn default() -> Self {
        Self::new(true)
    }
}

impl BlockBackend for StdBackend {
    fn open(&self, path: &Path) -> io::Result<FileHandle> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        Ok(FileHandle::new(file))
    }

    fn preallocate(&self, handle: &FileHandle, len: u64) -> io::Result<()> {
        if !self.preallocate {
            return Ok(());
        }
        self.file(handle)?.set_len(len)
    }

    fn allocate_and_zero(&self, handle: &FileHandle, len: u64) -> io::Result<()> {
        let file = self.file(handle)?;
        file.set_len(len)?;
        let chunk = ZERO_CHUNK.min(usize::try_from(len).unwrap_or(usize::MAX));
        if chunk == 0 {
            return Ok(());
        }
        let zeros = vec![0_u8; chunk];
        let mut offset = 0_u64;
        while offset < len {
            let remaining = len - offset;
            let n = usize::try_from(remaining.min(chunk as u64)).unwrap_or(chunk);
            platform::write_all_at(file, offset, &zeros[..n])?;
            offset += n as u64;
        }
        Ok(())
    }

    fn read_exact_at(&self, handle: &FileHandle, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        platform::read_exact_at(self.file(handle)?, offset, buf)
    }

    fn write_all_at(&self, handle: &FileHandle, offset: u64, buf: &[u8]) -> io::Result<()> {
        platform::write_all_at(self.file(handle)?, offset, buf)
    }

    fn sync(&self, handle: &FileHandle) -> io::Result<()> {
        self.file(handle)?.sync_all()
    }
}

/// The opened files of one torrent, addressed in torrent space.
///
/// Construct it with [`FileSet::open`], then read whole pieces or arbitrary ranges
/// without knowing which file they live in.
pub struct FileSet {
    layout: Arc<FileLayout>,
    backend: Arc<dyn BlockBackend>,
    handles: Vec<FileHandle>,
}

impl FileSet {
    /// Opens (and creates missing) files without preallocating.
    pub fn open(
        layout: Arc<FileLayout>,
        backend: Arc<dyn BlockBackend>,
        root: &Path,
    ) -> Result<Self, DiskError> {
        Self::open_with(layout, backend, root, Allocation::None)
    }

    pub(crate) fn open_with(
        layout: Arc<FileLayout>,
        backend: Arc<dyn BlockBackend>,
        root: &Path,
        allocation: Allocation,
    ) -> Result<Self, DiskError> {
        let mut handles = Vec::new();
        for spec in layout.files() {
            let full_path = join_under_root(root, &spec.path)?;
            if let Some(parent) = full_path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent).map_err(|err| DiskError::io(&spec.path, err))?;
            }
            let handle = backend
                .open(&full_path)
                .map_err(|err| DiskError::io(&spec.path, err))?;
            if spec.length > 0 {
                match allocation {
                    Allocation::None => {}
                    Allocation::Sparse => backend
                        .preallocate(&handle, spec.length)
                        .map_err(|err| DiskError::io(&spec.path, err))?,
                    Allocation::Zeroed => backend
                        .allocate_and_zero(&handle, spec.length)
                        .map_err(|err| DiskError::io(&spec.path, err))?,
                }
            }
            handles.push(handle);
        }
        Ok(Self {
            layout,
            backend,
            handles,
        })
    }

    /// The layout this set was opened for.
    pub fn layout(&self) -> &FileLayout {
        &self.layout
    }

    /// Reads exactly `buf.len()` bytes at torrent offset `pos`.
    pub fn read_exact_at(&self, pos: u64, buf: &mut [u8]) -> Result<(), DiskError> {
        let mut segments = Vec::new();
        self.layout.map_into(pos, buf.len() as u64, &mut segments)?;
        let mut cursor = 0_usize;
        for segment in &segments {
            let end = cursor.saturating_add(segment.len);
            let Some(destination) = buf.get_mut(cursor..end) else {
                return Err(DiskError::OutOfRange {
                    pos,
                    end: pos.saturating_add(buf.len() as u64),
                    total: self.layout.total_length(),
                });
            };
            let handle = self
                .handles
                .get(segment.file)
                .ok_or(DiskError::HandleType)?;
            let path = &self
                .layout
                .files()
                .get(segment.file)
                .ok_or(DiskError::HandleType)?
                .path;
            self.backend
                .read_exact_at(handle, segment.file_offset, destination)
                .map_err(|err| DiskError::io(path, err))?;
            cursor = end;
        }
        Ok(())
    }

    /// Writes all of `buf` at torrent offset `pos`.
    pub fn write_all_at(&self, pos: u64, buf: &[u8]) -> Result<(), DiskError> {
        let mut segments = Vec::new();
        self.layout.map_into(pos, buf.len() as u64, &mut segments)?;
        let mut cursor = 0_usize;
        for segment in &segments {
            let end = cursor.saturating_add(segment.len);
            let Some(source) = buf.get(cursor..end) else {
                return Err(DiskError::OutOfRange {
                    pos,
                    end: pos.saturating_add(buf.len() as u64),
                    total: self.layout.total_length(),
                });
            };
            let handle = self
                .handles
                .get(segment.file)
                .ok_or(DiskError::HandleType)?;
            let path = &self
                .layout
                .files()
                .get(segment.file)
                .ok_or(DiskError::HandleType)?
                .path;
            self.backend
                .write_all_at(handle, segment.file_offset, source)
                .map_err(|err| DiskError::io(path, err))?;
            cursor = end;
        }
        Ok(())
    }

    /// Flushes every file to stable storage.
    pub fn sync_all(&self) -> Result<(), DiskError> {
        for (spec, handle) in self.layout.files().iter().zip(&self.handles) {
            self.backend
                .sync(handle)
                .map_err(|err| DiskError::io(&spec.path, err))?;
        }
        Ok(())
    }
}

/// Joins a torrent-relative path onto the download root, refusing anything that
/// could escape it. Callers are expected to sanitize paths already; this is the
/// second line of defence before a byte touches the filesystem.
pub(crate) fn join_under_root(root: &Path, relative: &Path) -> Result<PathBuf, DiskError> {
    if relative.as_os_str().is_empty() {
        return Err(DiskError::UnsafePath {
            path: relative.to_path_buf(),
        });
    }
    for component in relative.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(DiskError::UnsafePath {
                    path: relative.to_path_buf(),
                });
            }
        }
    }
    Ok(root.join(relative))
}

#[cfg(unix)]
mod platform {
    use std::fs::File;
    use std::io;
    use std::os::unix::fs::FileExt;

    pub(super) fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        FileExt::read_exact_at(file, buf, offset)
    }

    pub(super) fn write_all_at(file: &File, offset: u64, buf: &[u8]) -> io::Result<()> {
        FileExt::write_all_at(file, buf, offset)
    }
}

#[cfg(windows)]
mod platform {
    use std::fs::File;
    use std::io;
    use std::os::windows::fs::FileExt;

    pub(super) fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut done = 0_usize;
        while done < buf.len() {
            match FileExt::seek_read(file, &mut buf[done..], offset + done as u64) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                Ok(n) => done += n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    pub(super) fn write_all_at(file: &File, offset: u64, buf: &[u8]) -> io::Result<()> {
        let mut done = 0_usize;
        while done < buf.len() {
            match FileExt::seek_write(file, &buf[done..], offset + done as u64) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
                Ok(n) => done += n,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::fs::File;
    use std::io::{self, Read, Seek, SeekFrom, Write};

    pub(super) fn read_exact_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut file = file.try_clone()?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buf)
    }

    pub(super) fn write_all_at(file: &File, offset: u64, buf: &[u8]) -> io::Result<()> {
        let mut file = file.try_clone()?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_handle_downcasts_to_its_payload() {
        let handle = FileHandle::new(5_u32);
        assert_eq!(handle.get::<u32>(), Some(&5));
        assert!(handle.get::<String>().is_none());

        // Cloning shares the payload, it does not re-wrap the `Arc`.
        let cloned = handle.clone();
        assert_eq!(cloned.get::<u32>(), Some(&5));
    }
}
