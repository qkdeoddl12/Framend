use serde::{Deserialize, Serialize};
pub mod exfat;
mod export;
mod formats;
pub mod native_video;
mod scan;
mod source;
pub use export::recover;
pub use scan::scan;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFingerprint {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub modified_seconds: i64,
    pub modified_nanos: i64,
    pub changed_seconds: i64,
    pub changed_nanos: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Jpeg,
    Raf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub offset: u64,
    pub length: u64,
    pub format: Format,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub camera: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryRequest {
    pub source: SourceFingerprint,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Started {
        source: SourceFingerprint,
    },
    Progress {
        scanned: u64,
        total: u64,
    },
    Candidate {
        candidate: Candidate,
    },
    Saved {
        candidate: Candidate,
        path: String,
        sha256: String,
    },
    Complete {
        count: usize,
        directory: Option<String>,
    },
}
