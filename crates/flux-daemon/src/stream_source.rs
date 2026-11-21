//! Bridges a torrent's verified pieces to the loopback streaming server.
//!
//! A read blocks until every piece covering the requested range is verified,
//! then reads through [`flux_disk::FileSet`]. Unverified bytes are never handed
//! to a player: the server's `Content-Length` contract depends on it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use flux_disk::{FileLayout as DiskLayout, FileSet, FileSpec, StdBackend};

use crate::TorrentId;
use crate::session::{SessionHandle, StreamLayout};
use crate::stream::{StreamError, StreamSource};

/// How often a waiting read re-checks the verified piece set.
const WAIT_POLL: Duration = Duration::from_millis(50);

/// `StreamSource` for one torrent.
pub struct TorrentStreamSource {
    layout: StreamLayout,
    session: SessionHandle,
    id: TorrentId,
    files: Arc<FileSet>,
}

impl TorrentStreamSource {
    /// Opens the torrent's files and prepares the source.
    ///
    /// # Errors
    ///
    /// Returns [`StreamError::Io`] when the disk layout is inconsistent or a
    /// file cannot be opened.
    pub fn new(
        session: SessionHandle,
        id: TorrentId,
        layout: StreamLayout,
    ) -> Result<Self, StreamError> {
        let specs: Vec<FileSpec> = layout
            .files
            .iter()
            .map(|file| FileSpec::new(PathBuf::from(&file.path), file.offset, file.length))
            .collect();
        let disk = DiskLayout::new(specs).map_err(|error| StreamError::Io(error.to_string()))?;
        let files = FileSet::open(
            Arc::new(disk),
            Arc::new(StdBackend::default()),
            &layout.root,
        )
        .map_err(|error| StreamError::Io(error.to_string()))?;
        Ok(Self {
            layout,
            session,
            id,
            files: Arc::new(files),
        })
    }

    fn file(&self, index: usize) -> Result<&crate::session::StreamFile, StreamError> {
        self.layout.files.get(index).ok_or(StreamError::UnknownFile)
    }
}

#[async_trait]
impl StreamSource for TorrentStreamSource {
    fn file_len(&self, file: usize) -> Option<u64> {
        self.layout.files.get(file).map(|file| file.length)
    }

    fn content_type(&self, file: usize) -> &'static str {
        let path = self
            .layout
            .files
            .get(file)
            .map(|file| file.path.as_str())
            .unwrap_or("");
        content_type(path)
    }

    async fn read(
        &self,
        file: usize,
        start: u64,
        buf: &mut [u8],
        wait: Duration,
    ) -> Result<(), StreamError> {
        let file = self.file(file)?;
        let end = start
            .checked_add(buf.len() as u64)
            .ok_or(StreamError::OutOfRange)?;
        if start > file.length || end > file.length {
            return Err(StreamError::OutOfRange);
        }
        if buf.is_empty() {
            return Ok(());
        }

        let piece_length = u64::from(self.layout.piece_length).max(1);
        let first = usize::try_from((file.offset + start) / piece_length)
            .map_err(|_| StreamError::OutOfRange)?;
        let last = usize::try_from((file.offset + end - 1) / piece_length)
            .map_err(|_| StreamError::OutOfRange)?;
        let deadline = Instant::now() + wait;

        loop {
            let state = self
                .session
                .piece_state(&self.id)
                .await
                .map_err(|error| StreamError::Io(error.to_string()))?;
            let ready = (first..=last).all(|piece| state.have.get(piece).unwrap_or(false));
            if ready {
                break;
            }
            if Instant::now() >= deadline {
                return Err(StreamError::NotAvailable);
            }
            tokio::time::sleep(WAIT_POLL).await;
        }

        let files = Arc::clone(&self.files);
        let position = file.offset + start;
        let length = buf.len();
        let data = tokio::task::spawn_blocking(move || {
            let mut data = vec![0_u8; length];
            files
                .read_exact_at(position, &mut data)
                .map(|()| data)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| StreamError::Io(error.to_string()))?
        .map_err(StreamError::Io)?;

        if data.len() != buf.len() {
            return Err(StreamError::Io("short read from storage".to_owned()));
        }
        buf.copy_from_slice(&data);
        Ok(())
    }
}

/// Best-effort media type from the file extension.
fn content_type(path: &str) -> &'static str {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "ts" | "m2ts" => "video/mp2t",
        "mpg" | "mpeg" => "video/mpeg",
        "flv" => "video/x-flv",
        "wmv" => "video/x-ms-wmv",
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "m4a" | "aac" => "audio/mp4",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::content_type;

    #[test]
    fn media_types_follow_extensions() {
        assert_eq!(content_type("movie.mkv"), "video/x-matroska");
        assert_eq!(content_type("MOVIE.MP4"), "video/mp4");
        assert_eq!(content_type("song.flac"), "audio/flac");
        assert_eq!(content_type("readme.txt"), "application/octet-stream");
        assert_eq!(content_type("no-extension"), "application/octet-stream");
    }
}
