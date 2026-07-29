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

/// Every method takes a `path` in one of two forms — see the per-method docs, which are what the
/// announced tool descriptors carry (only method doc comments reach the wire, not this one).
#[zyris::capability(name = "file_io", version = 1)]
pub trait FileIo {
    /// Stat a file or directory, returning its size, kind and modification time.
    ///
    /// `path` may be relative or absolute. A relative path (`notes/hello.txt`, `sub/dir`) resolves
    /// against the node's root directory. A path with a leading `/` is an absolute host path
    /// (`/home/allen/projects/notes.txt`) and is used as given. `.` and `..` are normalized.
    async fn stat(&self, path: String) -> zyris::Result<FileStat>;

    /// List the entries of a directory, sorted by name.
    ///
    /// `path` may be relative or absolute. A relative path (`notes`, `sub/dir`) resolves against
    /// the node's root directory; pass `.` or an empty string for the root itself. A path with a
    /// leading `/` is an absolute host path (`/home/allen/projects`) and is used as given. `.` and
    /// `..` are normalized.
    async fn list(&self, path: String) -> zyris::Result<Vec<DirEntry>>;

    /// Read a file; the head carries its stat, chunks carry the bytes. `offset` and `len` select a
    /// byte range, both optional.
    ///
    /// `path` may be relative or absolute. A relative path (`notes/hello.txt`) resolves against the
    /// node's root directory. A path with a leading `/` is an absolute host path
    /// (`/home/allen/projects/notes.txt`) and is used as given. `.` and `..` are normalized.
    #[zyris(uni_stream)]
    async fn read(
        &self,
        path: String,
        offset: Option<u64>,
        len: Option<u64>,
    ) -> zyris::Result<Streaming<FileStat, Chunk>>;

    /// Write a whole file from a datum, creating parent directories as needed. Fails if the file
    /// exists and `overwrite` is false.
    ///
    /// `path` may be relative or absolute. A relative path (`notes/hello.txt`) resolves against the
    /// node's root directory. A path with a leading `/` is an absolute host path
    /// (`/home/allen/projects/notes.txt`) and is used as given. `.` and `..` are normalized.
    async fn write(&self, path: String, data: Datum, overwrite: bool) -> zyris::Result<FileStat>;

    /// Remove a file or an empty directory.
    ///
    /// `path` may be relative or absolute. A relative path (`notes/hello.txt`) resolves against the
    /// node's root directory. A path with a leading `/` is an absolute host path
    /// (`/home/allen/projects/notes.txt`) and is used as given. `.` and `..` are normalized.
    async fn remove(&self, path: String) -> zyris::Result<()>;

    /// Create a directory, including any missing parents.
    ///
    /// `path` may be relative or absolute. A relative path (`notes/archive`) resolves against the
    /// node's root directory. A path with a leading `/` is an absolute host path
    /// (`/home/allen/projects/archive`) and is used as given. `.` and `..` are normalized.
    async fn mkdir(&self, path: String) -> zyris::Result<()>;
}
