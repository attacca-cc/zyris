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

/// What a unary [`FileIo::read`] returns: the file's stat plus the bytes of the requested range,
/// decoded as UTF-8.
///
/// Text rather than a [`Datum`](zyris::Datum) on purpose. A `Datum` carrying a blob over
/// [`INLINE_BLOB_MAX`](zyris::INLINE_BLOB_MAX) is moved onto its own stream by the connection
/// layer before the response goes out, which would put this tool right back on the stream path it
/// exists to avoid. A plain string is never detached. Callers wanting raw bytes, or a file larger
/// than one response can hold, want [`FileIo::read_stream`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileRead {
    pub stat: FileStat,
    /// The requested range decoded as UTF-8, with invalid sequences replaced by U+FFFD. Binary
    /// files therefore come back lossy — read them with `read_stream`.
    pub content: String,
    /// Byte offset `content` starts at.
    pub offset: u64,
    /// How many bytes of the file `content` covers. Not `content.len()`, which is the length after
    /// UTF-8 replacement.
    pub len: u64,
    /// True when `content` stops before the end of the file because the response limit was hit.
    /// Call again with `offset` = `offset + len` for the next slice.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Every method takes a `path` in one of two forms — see the per-method docs, which are what the
/// announced tool descriptors carry (only method doc comments reach the wire, not this one).
#[zyris::capability(name = "file_io", version = 2)]
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

    /// Read a file's text. `offset` and `len` select a byte range, both optional; omitting them
    /// reads from the start. Large files come back truncated with `truncated: true` — read on by
    /// passing `offset` = the previous `offset + len`.
    ///
    /// `path` may be relative or absolute. A relative path (`notes/hello.txt`) resolves against the
    /// node's root directory. A path with a leading `/` is an absolute host path
    /// (`/home/allen/projects/notes.txt`) and is used as given. `.` and `..` are normalized.
    async fn read(
        &self,
        path: String,
        offset: Option<u64>,
        len: Option<u64>,
    ) -> zyris::Result<FileRead>;

    /// Read a file as a byte stream; the head carries its stat, chunks carry the bytes. `offset`
    /// and `len` select a byte range, both optional.
    ///
    /// This is the bulk and binary path, and it costs a caller that can declare a stream (§4 of the
    /// protocol: the caller allocates the id and puts it in `req.stream`). A caller that cannot —
    /// notably an agent tool-caller invoking tools unary — wants [`FileIo::read`] instead, which
    /// returns text in the response itself.
    ///
    /// `path` may be relative or absolute. A relative path (`notes/hello.txt`) resolves against the
    /// node's root directory. A path with a leading `/` is an absolute host path
    /// (`/home/allen/projects/notes.txt`) and is used as given. `.` and `..` are normalized.
    #[zyris(uni_stream)]
    async fn read_stream(
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
