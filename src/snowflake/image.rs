use crate::snowflake::ABI;
use crate::snowflake::value::{Op, SymbolId, Value};
use crate::snowflake::world::{State, Transaction, World};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    #[error("invalid experimental image: {0}")]
    Invalid(String),
    #[error("image I/O: {0}")]
    Io(#[from] std::io::Error),
}

pub struct ImageStore(PathBuf);

impl ImageStore {
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self(directory.into())
    }

    fn slot(&self, index: usize) -> PathBuf {
        self.0.join(format!("slot-{index}.json"))
    }

    pub fn load(&self) -> Result<World, ImageError> {
        let mut found = false;
        let mut failure = None;
        let mut images = Vec::new();
        for index in 0..2 {
            let path = self.slot(index);
            if !path.exists() {
                continue;
            }
            found = true;
            match self.read(&path) {
                Ok(image) => images.push(image),
                Err(error) => failure = Some(error),
            }
        }
        images.sort_by_key(|image| image.generation);
        if let Some(image) = images.pop() {
            return Ok(World { state: image.state });
        }
        if !found {
            return Err(ImageError::NotFound);
        }
        Err(failure.expect("an existing slot was either valid or failed"))
    }

    pub fn save(
        &self,
        world: &World,
        transaction: Option<&mut Transaction>,
    ) -> Result<(), ImageError> {
        let mut selected = None;
        for index in 0..2 {
            if let Ok(image) = self.read(&self.slot(index))
                && selected
                    .as_ref()
                    .is_none_or(|(_, old): &(usize, Image)| image.generation > old.generation)
            {
                selected = Some((index, image));
            }
        }
        let (slot, generation) = match selected {
            None => (0, 0),
            Some((index, image)) => (
                1 - index,
                image
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| ImageError::Invalid("image generation exhausted".into()))?,
            ),
        };
        let mut state = transaction.map_or_else(
            || world.state.clone(),
            |transaction| transaction.committed().clone(),
        );
        validate(&mut state)?;
        let image = Image {
            abi: ABI,
            generation,
            checksum: checksum(generation, &state)?,
            state,
        };
        self.write(&self.slot(slot), &image)
    }

    fn read(&self, path: &Path) -> Result<Image, ImageError> {
        let mut image: Image = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|error| ImageError::Invalid(error.to_string()))?;
        if image.abi != ABI {
            return Err(ImageError::Invalid("incompatible ABI".into()));
        }
        if checksum(image.generation, &image.state)? != image.checksum {
            return Err(ImageError::Invalid("checksum mismatch".into()));
        }
        validate(&mut image.state)?;
        Ok(image)
    }

    fn write(&self, path: &Path, image: &Image) -> Result<(), ImageError> {
        std::fs::create_dir_all(&self.0)?;
        let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = File::create(&temporary)?;
            serde_json::to_writer(&mut file, image)
                .map_err(|error| ImageError::Invalid(error.to_string()))?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            std::fs::rename(&temporary, path)?;
            File::open(&self.0)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
}

fn checksum(generation: u64, state: &State) -> Result<u32, ImageError> {
    let bytes = serde_json::to_vec(&(ABI, generation, state))
        .map_err(|error| ImageError::Invalid(error.to_string()))?;
    let digest = Sha256::digest(bytes);
    Ok(u32::from_le_bytes(
        digest[..4].try_into().expect("digest has four bytes"),
    ))
}

fn present<T>(arena: &[Option<T>], id: u32) -> bool {
    arena.get(id as usize).is_some_and(Option::is_some)
}

fn valid_value(root: &Value, state: &State, symbols: u32) -> bool {
    let mut pending = vec![root];
    while let Some(value) = pending.pop() {
        match value {
            Value::Symbol(id) if id.0 >= symbols => return false,
            Value::List(values) => pending.extend(values),
            Value::Map(entries) => {
                pending.extend(entries.iter().flat_map(|(key, value)| [key, value]));
            }
            Value::Closure { chunk, captures } => {
                if state
                    .code
                    .get(chunk.0 as usize)
                    .and_then(Option::as_ref)
                    .is_none_or(|target| target.captures.len() != captures.len())
                    || captures.iter().any(|id| !present(&state.cells, id.0))
                {
                    return false;
                }
            }
            Value::Host(host) if !crate::snowflake::effects::valid_host(*host) => return false,
            Value::Float(value) if !value.is_finite() => return false,
            _ => {}
        }
    }
    true
}

fn validate(state: &mut State) -> Result<(), ImageError> {
    let names: Vec<_> = (0..)
        .map_while(|id| state.symbols.name(SymbolId(id)))
        .collect();
    let symbol_count = names.len() as u32;
    if names.iter().collect::<HashSet<_>>().len() != names.len() {
        return Err(ImageError::Invalid("duplicate symbol name".into()));
    }
    state.symbols.rebuild_index();
    let valid = |value| valid_value(value, state, symbol_count);
    let globals = state.globals.iter().all(|(name, binding)| {
        name.0 < symbol_count
            && valid(&binding.value)
            && binding.source.is_none_or(|id| present(&state.code, id.0))
    });
    let cells = state.cells.iter().flatten().all(&valid);
    let world = World {
        state: state.clone(),
    };
    let mut code = true;
    for chunk in state.code.iter().flatten() {
        world.validate_chunk(chunk).map_err(ImageError::Invalid)?;
        code &= chunk.constants.iter().all(&valid);
        code &= chunk.code.iter().all(|op| {
            !matches!(op,
            Op::GetGlobal(id) | Op::DefGlobal(id) | Op::SetGlobal(id) if id.0 >= symbol_count)
        });
    }
    let messages: HashSet<_> = state.messages.keys().copied().collect();
    let agent_refs = !state.agents.is_empty()
        && state
            .agents
            .iter()
            .all(|agent| agent.inbox.iter().all(|id| messages.contains(id)));
    let message_refs = state.messages.keys().all(|key| key.0 < state.next_message);
    if !(globals
        && cells
        && code
        && agent_refs
        && message_refs
        && crate::snowflake::effects::valid_prelude(&world))
    {
        return Err(ImageError::Invalid(
            "dangling or invalid state reference".into(),
        ));
    }
    Ok(())
}
