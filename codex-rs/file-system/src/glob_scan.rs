use codex_protocol::permissions::FileSystemGlobScan;
use codex_utils_path_uri::PathUri;
use serde::Deserialize;
use serde::Serialize;
use std::io;

/// Portable scan intent for an exec filesystem request. Intermediaries retain
/// foreign directories as URIs; only the receiving executor converts them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecFileSystemGlobScan {
    DatabaseEntries { directory: PathUri },
}

impl From<FileSystemGlobScan> for ExecFileSystemGlobScan {
    fn from(value: FileSystemGlobScan) -> Self {
        match value {
            FileSystemGlobScan::DatabaseEntries { directory } => Self::DatabaseEntries {
                directory: PathUri::from_abs_path(&directory),
            },
        }
    }
}

impl TryFrom<ExecFileSystemGlobScan> for FileSystemGlobScan {
    type Error = io::Error;

    fn try_from(value: ExecFileSystemGlobScan) -> Result<Self, Self::Error> {
        Ok(match value {
            ExecFileSystemGlobScan::DatabaseEntries { directory } => Self::DatabaseEntries {
                directory: directory.to_abs_path()?,
            },
        })
    }
}
