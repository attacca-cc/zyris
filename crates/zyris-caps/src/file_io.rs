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

/// Every `path` follows one rule: a leading `/` is an absolute host path
/// (`/home/allen/projects`), anything else is relative to the capability's root
/// (`notes/hello.txt`). `.` and `..` are normalized lexically.
#[zyris::capability(name = "file_io", version = 1)]
pub trait FileIo {
    /// Stat a path; absolute with a leading `/`, otherwise relative to the root.
    async fn stat(&self, path: String) -> zyris::Result<FileStat>;

    /// List the entries of a directory; absolute with a leading `/`, otherwise relative to the root.
    async fn list(&self, path: String) -> zyris::Result<Vec<DirEntry>>;

    /// Read a file; the head carries its stat, chunks carry the bytes.
    ///
    /// Absolute with a leading `/`, otherwise relative to the root.
    #[zyris(uni_stream)]
    async fn read(
        &self,
        path: String,
        offset: Option<u64>,
        len: Option<u64>,
    ) -> zyris::Result<Streaming<FileStat, Chunk>>;

    /// Write a whole file from a datum; absolute with a leading `/`, otherwise relative to the root.
    async fn write(&self, path: String, data: Datum, overwrite: bool) -> zyris::Result<FileStat>;

    /// Remove a file or empty directory; absolute with a leading `/`, otherwise relative to the root.
    async fn remove(&self, path: String) -> zyris::Result<()>;

    /// Create a directory (and parents); absolute with a leading `/`, otherwise relative to the root.
    async fn mkdir(&self, path: String) -> zyris::Result<()>;
}
