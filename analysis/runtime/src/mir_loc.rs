use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::path::PathBuf;
use std::sync::RwLock;

lazy_static! {
    static ref MIR_LOC_FILE_PATH: RwLock<Option<PathBuf>> = RwLock::new(None);
}

pub fn set_file(file_path: &str) {
    *MIR_LOC_FILE_PATH.write().unwrap() = Some(PathBuf::from(file_path));
}

lazy_static! {
    pub(crate) static ref MIR_LOCS: Metadata = {
        let path = MIR_LOC_FILE_PATH
            .read()
            .expect("MIR_LOC_FILE_PATH was locked")
            .clone()
            .expect("MIR_LOC_FILE_PATH not initialized by the instrumented code");
        let file =
            File::open(&path).expect(&format!("Could not open span file: {:?}", path.to_str()));
        bincode::deserialize_from(file).expect("Error deserializing span file")
    };
}

pub fn get(index: MirLocId) -> Option<&'static MirLoc> {
    if MIR_LOC_FILE_PATH.read().unwrap().is_some() {
        Some(&MIR_LOCS.locs[index as usize])
    } else {
        None
    }
}

pub type MirLocId = u32;

#[derive(Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct DefPathHash(pub u64, pub u64);

impl fmt::Debug for DefPathHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", MIR_LOCS.functions[self])
    }
}

impl From<(u64, u64)> for DefPathHash {
    fn from(other: (u64, u64)) -> Self {
        Self(other.0, other.1)
    }
}

impl Into<(u64, u64)> for DefPathHash {
    fn into(self) -> (u64, u64) {
        (self.0, self.1)
    }
}

#[derive(Debug, Clone, Copy, Hash, Serialize, Deserialize, PartialEq, Eq)]
pub enum RefKind {
    Ref(usize),
    Raw(usize),
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Clone, Copy)]
pub enum EventMetadata {
    CopyPtr(usize, RefKind),
    CopyRef(usize, RefKind),
    Field(usize),
    Hook(usize),
    Generic,
}

impl EventMetadata {
    pub fn source(&self) -> Option<RefKind> {
        match &self {
            EventMetadata::CopyPtr(_dest, src) => Some(*src),
            EventMetadata::CopyRef(_dest, src) => Some(*src),
            _ => None
        }
    }

    pub fn dest(&self) -> Option<RefKind> {
        match &self {
            EventMetadata::CopyPtr(dest, _src) => Some(RefKind::Ref(*dest)),
            EventMetadata::CopyRef(dest, _src) => Some(RefKind::Ref(*dest)),
            EventMetadata::Hook(dest) => Some(RefKind::Ref(*dest)),
            _ => None
        }
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct MirLoc {
    pub body_def: DefPathHash,
    pub basic_block_idx: usize,
    pub statement_idx: usize,
    pub metadata: EventMetadata,
}

impl fmt::Debug for MirLoc {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{:?}:{}:{}",
            self.body_def, self.basic_block_idx, self.statement_idx
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub locs: Vec<MirLoc>,
    pub functions: HashMap<DefPathHash, String>,
}
