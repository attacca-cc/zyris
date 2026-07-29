use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::{Chunk, Datum, Streaming};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileStat {
    pub path: String,
    pub size: u64,
    pub is_dir: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

#[zyris::capability(name = "file_io", version = 1)]
pub trait FileIo {
    /// Stat a path.
    async fn stat(&self, path: String) -> zyris::Result<FileStat>;

    /// List the entries of a directory.
    async fn list(&self, path: String) -> zyris::Result<Vec<DirEntry>>;

    /// Read a file; the head carries its stat, chunks carry the bytes.
    #[zyris(uni_stream)]
    async fn read(
        &self,
        path: String,
        offset: Option<u64>,
        len: Option<u64>,
    ) -> zyris::Result<Streaming<FileStat, Chunk>>;

    /// Write a whole file from a datum.
    async fn write(&self, path: String, data: Datum, overwrite: bool) -> zyris::Result<FileStat>;

    /// Remove a file or empty directory.
    async fn remove(&self, path: String) -> zyris::Result<()>;

    /// Create a directory (and parents).
    async fn mkdir(&self, path: String) -> zyris::Result<()>;
}
