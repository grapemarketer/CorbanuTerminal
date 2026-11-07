//! Provenance for Linux expansion of generated database-entry deny globs.
//! The ordinary pattern remains on the wire and in read-time matchers.
use super::FileSystemPath;
use codex_utils_absolute_path::AbsolutePathBuf;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(tag = "type")]
pub enum FileSystemGlobScan {
    /// Only direct entries of this separate runtime database folder. Keeping
    /// its literal path avoids mistaking escaped glob syntax for a scan root.
    DatabaseEntries { directory: AbsolutePathBuf },
}

/// One resolved deny glob and its expansion intent. Equality and ordering
/// include the intent so deduplication cannot drop a generic recursive rule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FileSystemDenyGlob {
    pub pattern: String,
    pub scan: Option<FileSystemGlobScan>,
}

impl FileSystemDenyGlob {
    /// Validate the generated hint before using it to bound a Linux scan.
    /// Inconsistent metadata must fail closed rather than narrow an unrelated
    /// rule or scan a different directory.
    pub fn database_entries_directory(&self) -> Result<Option<&AbsolutePathBuf>, &'static str> {
        match &self.scan {
            Some(FileSystemGlobScan::DatabaseEntries { directory }) => {
                if self.pattern != database_entries_pattern(directory) {
                    return Err("database-entry scan does not match its deny pattern");
                }
                Ok(Some(directory))
            }
            None => Ok(None),
        }
    }
}

impl FileSystemPath {
    /// A per-entry denial for a separate runtime database folder (#732).
    /// Generic globs, including identical user-authored patterns, have no hint.
    pub fn database_entries(directory: AbsolutePathBuf) -> Self {
        Self::GlobPattern {
            pattern: database_entries_pattern(&directory),
            scan: Some(FileSystemGlobScan::DatabaseEntries { directory }),
        }
    }
}

fn database_entries_pattern(directory: &AbsolutePathBuf) -> String {
    let escaped: String = directory
        .to_string_lossy()
        .chars()
        .map(|ch| match ch {
            '*' | '?' | '[' | '{' | '}' => format!("[{ch}]"),
            other => other.to_string(),
        })
        .collect();
    std::path::PathBuf::from(escaped)
        .join("*.sqlite*")
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
#[path = "glob_scan_tests.rs"]
mod tests;
