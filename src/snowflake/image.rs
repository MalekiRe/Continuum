use crate::snowflake::ABI;
use crate::snowflake::world::{State, Transaction, World};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
struct Image {
    abi: u64,
    generation: u64,
    checksum: u32,
    state: State,
}

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("no experimental image exists")]
    NotFound,
    #[error("incompatible experimental image")]
    Incompatible,
    #[error("invalid experimental image: {0}")]
    Invalid(String),
    #[error("image I/O: {0}")]
    Io(#[from] std::io::Error),
}

pub struct ImageStore {
    directory: PathBuf,
}

impl ImageStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn load(&self) -> Result<World, ImageError> {
        todo!("validate both slots and select the highest valid generation")
    }

    pub fn save(
        &self,
        _world: &World,
        _transaction: Option<&mut Transaction>,
    ) -> Result<(), ImageError> {
        todo!("atomically write the inactive slot from the committed view")
    }

    fn read(&self, _path: &Path) -> Result<Image, ImageError> {
        todo!("read, checksum, ABI-check, and validate one image")
    }

    fn write(&self, _path: &Path, _image: &Image) -> Result<(), ImageError> {
        todo!("sync a temporary file and atomically rename it")
    }
}

fn checksum(_state: &State) -> Result<u32, ImageError> {
    todo!("compute a small corruption checksum over canonical serialized state")
}

fn validate(_state: &mut State) -> Result<(), ImageError> {
    let _ = ABI;
    todo!("rebuild symbol indexes and validate every current-format reference")
}
