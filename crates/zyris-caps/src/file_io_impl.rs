use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use bytes::Bytes;
use zyris::{Blob, Chunk, Datum, ErrorCode, Streaming, WireError};

use crate::file_io::{DirEntry, FileIo, FileStat};

const READ_CHUNK: usize = 128 * 1024;

pub struct LocalFileIo {
    root: PathBuf,
}

impl LocalFileIo {
    pub fn rooted(root: impl Into<PathBuf>) -> Self {
        LocalFileIo { root: root.into() }
    }

    fn resolve(&self, path: &str) -> zyris::Result<PathBuf> {
        let requested = Path::new(path);
        let mut resolved = self.root.clone();
        for component in requested.components() {
            match component {
                Component::Normal(part) => resolved.push(part),
                Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
                Component::ParentDir => {
                    return Err(WireError::new(
                        ErrorCode::ForbiddenScope,
                        "path escapes the file_io root",
                    ));
                }
            }
        }
        Ok(resolved)
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string()
    }
}

fn io_err(err: std::io::Error) -> WireError {
    let code = match err.kind() {
        std::io::ErrorKind::NotFound => ErrorCode::Other("not_found".into()),
        std::io::ErrorKind::PermissionDenied => ErrorCode::ForbiddenScope,
        _ => ErrorCode::Internal,
    };
    WireError::new(code, err.to_string())
}

async fn stat_path(display: String, path: &Path) -> zyris::Result<FileStat> {
    let meta = tokio::fs::metadata(path).await.map_err(io_err)?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);
    Ok(FileStat {
        path: display,
        size: meta.len(),
        is_dir: meta.is_dir(),
        modified_unix_ms: modified,
    })
}

#[zyris::async_trait]
impl FileIo for LocalFileIo {
    async fn stat(&self, path: String) -> zyris::Result<FileStat> {
        let resolved = self.resolve(&path)?;
        stat_path(path, &resolved).await
    }

    async fn list(&self, path: String) -> zyris::Result<Vec<DirEntry>> {
        let resolved = self.resolve(&path)?;
        let mut read_dir = tokio::fs::read_dir(&resolved).await.map_err(io_err)?;
        let mut entries = Vec::new();
        while let Some(entry) = read_dir.next_entry().await.map_err(io_err)? {
            let meta = entry.metadata().await.map_err(io_err)?;
            entries.push(DirEntry {
                name: entry.file_name().to_string_lossy().to_string(),
                is_dir: meta.is_dir(),
                size: meta.len(),
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    async fn read(
        &self,
        path: String,
        offset: Option<u64>,
        len: Option<u64>,
    ) -> zyris::Result<Streaming<FileStat, Chunk>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};

        let resolved = self.resolve(&path)?;
        let stat = stat_path(path, &resolved).await?;
        let mut file = tokio::fs::File::open(&resolved).await.map_err(io_err)?;
        if let Some(offset) = offset {
            file.seek(std::io::SeekFrom::Start(offset)).await.map_err(io_err)?;
        }
        let mut remaining = len.map(|l| l as usize);
        let items = futures_util::stream::unfold(
            (file, remaining.take()),
            move |(mut file, mut remaining)| async move {
                let cap = match remaining {
                    Some(0) => return None,
                    Some(r) => r.min(READ_CHUNK),
                    None => READ_CHUNK,
                };
                let mut buf = vec![0u8; cap];
                match file.read(&mut buf).await {
                    Ok(0) => None,
                    Ok(n) => {
                        buf.truncate(n);
                        if let Some(r) = remaining.as_mut() {
                            *r -= n;
                        }
                        Some((Ok(Chunk::new(Bytes::from(buf))), (file, remaining)))
                    }
                    Err(e) => Some((Err(io_err(e)), (file, Some(0)))),
                }
            },
        );
        Ok(Streaming::new(stat, items))
    }

    async fn write(
        &self,
        path: String,
        data: Datum,
        overwrite: bool,
    ) -> zyris::Result<FileStat> {
        let resolved = self.resolve(&path)?;
        let bytes = match data {
            Datum::Text { text, .. } => Bytes::from(text.into_bytes()),
            Datum::File { blob, .. } | Datum::Image { blob, .. } => match blob {
                Blob::Inline(bytes) => bytes,
                Blob::Attachment(_) => {
                    return Err(WireError::invalid_params(
                        "attachment blobs are not supported by file_io.write yet",
                    ))
                }
            },
        };
        if !overwrite && tokio::fs::try_exists(&resolved).await.map_err(io_err)? {
            return Err(WireError::invalid_params(
                "file exists and overwrite is false",
            ));
        }
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(io_err)?;
        }
        tokio::fs::write(&resolved, &bytes).await.map_err(io_err)?;
        stat_path(self.relative(&resolved), &resolved).await
    }

    async fn remove(&self, path: String) -> zyris::Result<()> {
        let resolved = self.resolve(&path)?;
        let meta = tokio::fs::metadata(&resolved).await.map_err(io_err)?;
        if meta.is_dir() {
            tokio::fs::remove_dir(&resolved).await.map_err(io_err)
        } else {
            tokio::fs::remove_file(&resolved).await.map_err(io_err)
        }
    }

    async fn mkdir(&self, path: String) -> zyris::Result<()> {
        let resolved = self.resolve(&path)?;
        tokio::fs::create_dir_all(&resolved).await.map_err(io_err)
    }
}
