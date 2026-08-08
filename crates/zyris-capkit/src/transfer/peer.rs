//! TODO(Task 1.5): `peer_transfer`의 참조 구현.
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct TransferConfig {
    pub inbox: PathBuf,
    pub undo: PathBuf,
    pub audit: Option<PathBuf>,
    pub max_file_bytes: u64,
    pub max_inbox_bytes: u64,
}

impl Default for TransferConfig {
    fn default() -> TransferConfig {
        TransferConfig {
            inbox: PathBuf::from("."),
            undo: PathBuf::from("."),
            audit: None,
            max_file_bytes: 8 * 1024 * 1024 * 1024,
            max_inbox_bytes: 32 * 1024 * 1024 * 1024,
        }
    }
}

pub struct LocalPeerTransfer {
    _config: TransferConfig,
}

impl LocalPeerTransfer {
    pub fn sender(root: PathBuf) -> LocalPeerTransfer {
        LocalPeerTransfer {
            _config: TransferConfig { inbox: root.clone(), undo: root, ..Default::default() },
        }
    }
}
