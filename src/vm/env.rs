use crate::ids::QualifiedName;
use crate::vm::value::{Function, Value, collect_captured_environments};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("invalid qualified name: {0}")]
    InvalidName(String),
    #[error("namespace '{0}' is protected and cannot be modified")]
    Protected(String),
    #[error("native binding '{0}' cannot be modified")]
    NativeBinding(String),
    #[error("namespace '{0}' not found")]
    NamespaceNotFound(String),
    #[error("binding '{0}' not found")]
    BindingNotFound(String),
    #[error("closure environment {0} is missing")]
    MissingEnvironment(EnvironmentId),
    #[error("data family '{0}' not found")]
    FamilyNotFound(String),
}

fn qualified_parts(name: &str) -> Result<(&str, &str), EnvError> {
    match name.split_once('/') {
        Some((namespace, local)) if !namespace.is_empty() && !local.is_empty() => {
            Ok((namespace, local))
        }
        _ => Err(EnvError::InvalidName(name.into())),
    }
}

/// A snapshot of a binding at one point in history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingRecord {
    pub value: Value,
    pub timestamp: String,
    pub version: u64,
}

/// A tagged data variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataVariant {
    pub name: String,
    pub fields: Vec<String>,
}

/// A tagged data family.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataFamily {
    /// Canonical `namespace/name` identity.
    pub name: QualifiedName,
    pub variants: Vec<DataVariant>,
    /// Exact qualified bindings installed for this definition.
    #[serde(default)]
    pub generated_bindings: Vec<QualifiedName>,
}

/// A single namespace holding named bindings with version history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Namespace {
    pub(crate) bindings: IndexMap<String, Value>,
    pub(crate) history: IndexMap<String, Vec<BindingRecord>>,
    #[serde(default)]
    pub(crate) sources: IndexMap<String, String>,
    pub(crate) next_version: u64,
    pub(crate) protected: bool,
    pub(crate) data_families: IndexMap<String, DataFamily>,
}

impl Namespace {
    pub fn new(name: &str) -> Self {
        let protected =
            name == "system" || name == "kernel" || name == "inspect" || name == "control";
        Namespace {
            bindings: IndexMap::new(),
            history: IndexMap::new(),
            sources: IndexMap::new(),
            next_version: 1,
            protected,
            data_families: IndexMap::new(),
        }
    }

    fn remember(&mut self, name: &str, value: Value) {
        let history = self.history.entry(name.into()).or_default();
        history.push(BindingRecord {
            value,
            timestamp: chrono::Utc::now().to_rfc3339(),
            version: self.next_version,
        });
        self.next_version += 1;
        if history.len() > 32 {
            history.remove(0);
        }
    }

    pub fn define(&mut self, name: &str, value: Value) {
        if let Some(old) = self.bindings.get(name).cloned() {
            self.remember(name, old);
        }
        self.bindings.insert(name.into(), value);
    }

    pub fn undefine(&mut self, name: &str) -> Option<Value> {
        if self.protected {
            return None;
        }
        let removed = self.bindings.shift_remove(name);
        self.sources.shift_remove(name);
        if let Some(value) = removed.clone() {
            self.remember(name, value);
        }
        removed
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.bindings.get(name)
    }

    pub fn history(&self, name: &str) -> &[BindingRecord] {
        self.history.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn list_bindings(&self) -> Vec<String> {
        let mut names: Vec<String> = self.bindings.keys().cloned().collect();
        names.sort();
        names
    }
}

/// Stable identifiers used by the serializable lexical arena.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EnvironmentId(u64);

impl EnvironmentId {
    pub const ROOT: Self = Self(0);
    const FIRST_ALLOCATED: Self = Self(1);
    fn take_and_advance(&mut self) -> Self {
        let current = *self;
        self.0 += 1;
        current
    }
}

impl std::fmt::Display for EnvironmentId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CellId(u64);

impl CellId {
    const FIRST: Self = Self(1);
    fn take_and_advance(&mut self) -> Self {
        let current = *self;
        self.0 += 1;
        current
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LexicalEnvironment {
    pub(crate) parent: Option<EnvironmentId>,
    pub(crate) bindings: IndexMap<String, CellId>,
}

/// Serializable storage for lexical scopes and their mutable binding cells.
///
/// Environments point at cells rather than containing values directly. A
/// closure therefore captures an environment id, and all closures which see a
/// binding share the same cell (including after snapshot recovery).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LexicalArena {
    pub(crate) environments: IndexMap<EnvironmentId, LexicalEnvironment>,
    pub(crate) cells: IndexMap<CellId, Value>,
    next_environment_id: EnvironmentId,
    next_cell_id: CellId,
}

impl Default for LexicalArena {
    fn default() -> Self {
        let mut environments = IndexMap::new();
        environments.insert(
            EnvironmentId::ROOT,
            LexicalEnvironment {
                parent: None,
                bindings: IndexMap::new(),
            },
        );
        Self {
            environments,
            cells: IndexMap::new(),
            next_environment_id: EnvironmentId::FIRST_ALLOCATED,
            next_cell_id: CellId::FIRST,
        }
    }
}

impl LexicalArena {
    fn validate(&self, current: EnvironmentId) -> Result<(), String> {
        let root = self
            .environments
            .get(&EnvironmentId::ROOT)
            .ok_or_else(|| "lexical arena is missing its root environment".to_string())?;
        if root.parent.is_some() {
            return Err("lexical root environment has a parent".into());
        }
        if !self.environments.contains_key(&current) {
            return Err(format!("active lexical environment {current} is missing"));
        }
        if self.next_environment_id.0 == u64::MAX
            || self
                .environments
                .keys()
                .any(|id| id.0 >= self.next_environment_id.0)
        {
            return Err("lexical environment allocator is stale or exhausted".into());
        }
        if self.next_cell_id.0 == u64::MAX
            || self.cells.keys().any(|id| id.0 >= self.next_cell_id.0)
        {
            return Err("lexical cell allocator is stale or exhausted".into());
        }
        let mut validated = HashSet::from([EnvironmentId::ROOT]);
        for (&id, environment) in &self.environments {
            if environment
                .bindings
                .values()
                .any(|cell| !self.cells.contains_key(cell))
            {
                return Err(format!(
                    "lexical environment {id} references a missing cell"
                ));
            }
            let (mut cursor, mut path, mut visiting) = (id, Vec::new(), HashSet::new());
            while !validated.contains(&cursor) {
                if !visiting.insert(cursor) {
                    return Err(format!("lexical parent cycle at environment {cursor}"));
                }
                path.push(cursor);
                let frame = self
                    .environments
                    .get(&cursor)
                    .ok_or_else(|| format!("lexical parent environment {cursor} is missing"))?;
                cursor = frame
                    .parent
                    .ok_or_else(|| format!("lexical environment {cursor} is detached"))?;
            }
            validated.extend(path);
        }
        Ok(())
    }

    fn allocate_environment(&mut self, parent: EnvironmentId) -> EnvironmentId {
        let id = self.next_environment_id.take_and_advance();
        self.environments.insert(
            id,
            LexicalEnvironment {
                parent: Some(parent),
                bindings: IndexMap::new(),
            },
        );
        id
    }

    fn define(&mut self, environment: EnvironmentId, name: &str, value: Value) {
        let existing = self
            .environments
            .get(&environment)
            .and_then(|frame| frame.bindings.get(name))
            .copied();
        if let Some(cell) = existing {
            self.cells.insert(cell, value);
            return;
        }

        let cell = self.next_cell_id.take_and_advance();
        self.cells.insert(cell, value);
        self.environments
            .get_mut(&environment)
            .expect("active lexical environment must exist")
            .bindings
            .insert(name.to_string(), cell);
    }

    fn find_cell(&self, mut environment: EnvironmentId, name: &str) -> Option<CellId> {
        loop {
            let frame = self.environments.get(&environment)?;
            if let Some(cell) = frame.bindings.get(name) {
                return Some(*cell);
            }
            environment = frame.parent?;
        }
    }
}

/// The namespaces and current cursor into the lexical arena.
#[derive(Debug, Clone, Serialize)]
pub struct EnvRef {
    pub(crate) namespaces: Arc<IndexMap<String, Namespace>>,
    #[serde(default)]
    pub(crate) lexical: LexicalArena,
    #[serde(default)]
    current_environment: EnvironmentId,
}

#[derive(Deserialize)]
struct EnvRefWire {
    namespaces: Arc<IndexMap<String, Namespace>>,
    #[serde(default)]
    lexical: LexicalArena,
    #[serde(default)]
    current_environment: EnvironmentId,
}

fn validate_namespaces(namespaces: &IndexMap<String, Namespace>) -> Result<(), String> {
    for (name, namespace) in namespaces {
        let protected = matches!(name.as_str(), "system" | "kernel" | "inspect" | "control");
        let empty_local = namespace
            .bindings
            .keys()
            .chain(namespace.sources.keys())
            .chain(namespace.history.keys())
            .chain(namespace.data_families.keys())
            .any(String::is_empty);
        let invalid_history = namespace.history.values().any(|records| {
            records.len() > 32
                || records
                    .iter()
                    .any(|record| record.version >= namespace.next_version)
                || records
                    .windows(2)
                    .any(|pair| pair[0].version >= pair[1].version)
        });
        let invalid_family = namespace.data_families.iter().any(|(local, family)| {
            family.name.as_str() != format!("{name}/{local}")
                || family
                    .generated_bindings
                    .iter()
                    .any(|binding| qualified_parts(binding.as_str()).is_err())
        });
        if name.is_empty()
            || name.contains('/')
            || namespace.protected != protected
            || empty_local
            || namespace.next_version == 0
            || namespace.next_version == u64::MAX
            || invalid_history
            || invalid_family
        {
            return Err(format!("namespace '{name}' has invalid serialized state"));
        }
    }
    Ok(())
}

fn validate_captured_environment_ids(
    namespaces: &IndexMap<String, Namespace>,
    lexical: &LexicalArena,
) -> Result<(), String> {
    let mut captured = HashSet::new();
    for namespace in namespaces.values() {
        collect_captured_environments(namespace.bindings.values(), &mut captured);
        collect_captured_environments(
            namespace
                .history
                .values()
                .flatten()
                .map(|record| &record.value),
            &mut captured,
        );
    }
    collect_captured_environments(lexical.cells.values(), &mut captured);
    if let Some(missing) = captured
        .into_iter()
        .find(|id| !lexical.environments.contains_key(id))
    {
        return Err(format!(
            "captured lexical environment {missing} does not exist"
        ));
    }
    Ok(())
}

impl<'de> Deserialize<'de> for EnvRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = EnvRefWire::deserialize(deserializer)?;
        validate_namespaces(&wire.namespaces).map_err(serde::de::Error::custom)?;
        wire.lexical
            .validate(wire.current_environment)
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            namespaces: wire.namespaces,
            lexical: wire.lexical,
            current_environment: wire.current_environment,
        })
    }
}

impl Default for EnvRef {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvRef {
    pub(crate) fn validate_captured_environments(&self) -> Result<(), String> {
        validate_captured_environment_ids(&self.namespaces, &self.lexical)
    }

    pub fn new() -> Self {
        let mut namespaces = IndexMap::new();
        namespaces.insert("system".into(), Namespace::new("system"));
        namespaces.insert("inspect".into(), Namespace::new("inspect"));
        namespaces.insert("control".into(), Namespace::new("control"));
        namespaces.insert("kernel".into(), Namespace::new("kernel"));
        EnvRef {
            namespaces: Arc::new(namespaces),
            lexical: LexicalArena::default(),
            current_environment: EnvironmentId::ROOT,
        }
    }

    pub fn lookup(&self, symbol: &str) -> Option<&Value> {
        if symbol.contains('/') {
            let (namespace, name) = qualified_parts(symbol).ok()?;
            return self.namespaces.get(namespace)?.get(name);
        }
        self.lexical
            .find_cell(self.current_environment, symbol)
            .and_then(|cell| self.lexical.cells.get(&cell))
            .or_else(|| {
                self.namespaces
                    .get("user")
                    .and_then(|namespace| namespace.get(symbol))
            })
            .or_else(|| {
                self.namespaces
                    .get("kernel")
                    .and_then(|namespace| namespace.get(symbol))
            })
    }

    pub fn define(&mut self, qualified_name: &str, value: Value) -> Result<(), EnvError> {
        let (namespace, name) = qualified_parts(qualified_name)?;
        let ns_name = namespace.to_string();

        let ns = Arc::make_mut(&mut self.namespaces)
            .entry(ns_name.clone())
            .or_insert_with(|| Namespace::new(&ns_name));
        if ns.protected {
            return Err(EnvError::Protected(ns_name));
        }
        if matches!(ns.get(name), Some(Value::Function(Function::Native { .. }))) {
            return Err(EnvError::NativeBinding(qualified_name.into()));
        }

        ns.define(name, value);
        Ok(())
    }

    pub fn store_source(&mut self, qualified_name: &str, source: String) -> Result<(), EnvError> {
        let (namespace, name) = qualified_parts(qualified_name)?;
        Arc::make_mut(&mut self.namespaces)
            .get_mut(namespace)
            .ok_or_else(|| EnvError::NamespaceNotFound(namespace.into()))?
            .sources
            .insert(name.into(), source);
        Ok(())
    }

    pub fn source(&self, qualified_name: &str) -> Option<&str> {
        let (namespace, name) = qualified_parts(qualified_name).ok()?;
        self.namespaces
            .get(namespace)?
            .sources
            .get(name)
            .map(String::as_str)
    }

    /// Force define, bypassing protection checks (for kernel use only).
    pub(crate) fn force_define(&mut self, qualified_name: &str, value: Value) {
        let Ok((namespace, name)) = qualified_parts(qualified_name) else {
            return;
        };
        Arc::make_mut(&mut self.namespaces)
            .entry(namespace.into())
            .or_insert_with(|| Namespace::new(namespace))
            .bindings
            .insert(name.into(), value);
    }

    pub fn undefine(&mut self, qualified_name: &str) -> Result<(), EnvError> {
        let (namespace, name) = qualified_parts(qualified_name)?;
        let ns = Arc::make_mut(&mut self.namespaces)
            .get_mut(namespace)
            .ok_or_else(|| EnvError::NamespaceNotFound(namespace.into()))?;
        if ns.protected {
            return Err(EnvError::Protected(namespace.into()));
        }
        if matches!(ns.get(name), Some(Value::Function(Function::Native { .. }))) {
            return Err(EnvError::NativeBinding(qualified_name.into()));
        }
        ns.undefine(name)
            .ok_or_else(|| EnvError::BindingNotFound(qualified_name.into()))?;
        Ok(())
    }

    /// Allocate a lexical child of the currently active environment.
    pub(crate) fn push_frame(&mut self) {
        self.current_environment = self.lexical.allocate_environment(self.current_environment);
    }

    pub(crate) fn set_lexical(&mut self, name: &str, value: Value) {
        self.lexical.define(self.current_environment, name, value);
    }

    pub(crate) fn set_existing_lexical(&mut self, name: &str, value: Value) -> bool {
        let Some(cell) = self.lexical.find_cell(self.current_environment, name) else {
            return false;
        };
        self.lexical.cells.insert(cell, value);
        true
    }

    pub(crate) fn import_legacy_frames(
        &mut self,
        frames: Vec<IndexMap<String, Value>>,
    ) -> EnvironmentId {
        let mut parent = EnvironmentId::ROOT;
        for frame in frames {
            let environment = self.lexical.allocate_environment(parent);
            for (name, value) in frame {
                self.lexical.define(environment, &name, value);
            }
            parent = environment;
        }
        parent
    }

    pub fn lexical_arena_counts(&self) -> (usize, usize) {
        (self.lexical.environments.len(), self.lexical.cells.len())
    }

    pub fn binding_history_len(&self, namespace: &str, name: &str) -> usize {
        self.namespaces
            .get(namespace)
            .map_or(0, |namespace| namespace.history(name).len())
    }

    pub(crate) fn current_environment(&self) -> EnvironmentId {
        self.current_environment
    }

    pub(crate) fn activate_environment(
        &mut self,
        environment: EnvironmentId,
    ) -> Result<(), EnvError> {
        if !self.lexical.environments.contains_key(&environment) {
            return Err(EnvError::MissingEnvironment(environment));
        }
        self.current_environment = environment;
        Ok(())
    }

    /// Allocate a call frame parented directly to a closure's captured scope.
    pub(crate) fn push_call_frame(&mut self, captured: EnvironmentId) -> Result<(), EnvError> {
        if !self.lexical.environments.contains_key(&captured) {
            return Err(EnvError::MissingEnvironment(captured));
        }
        self.current_environment = self.lexical.allocate_environment(captured);
        Ok(())
    }

    pub fn namespace_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.namespaces.keys().cloned().collect();
        names.sort();
        names
    }

    fn qualify_data_family(family_name: &str) -> Result<QualifiedName, EnvError> {
        let name = if family_name.contains('/') {
            family_name.to_string()
        } else {
            format!("user/{}", family_name)
        };
        qualified_parts(&name)?;
        Ok(QualifiedName::new(name))
    }

    /// Replace family metadata and remove only constructors generated by the
    /// previous definition of this exact qualified family.
    pub fn set_data_family(&mut self, mut family: DataFamily) -> Result<(), EnvError> {
        let qualified = Self::qualify_data_family(family.name.as_str())?;
        family.name = qualified.clone();
        let (namespace, name) = qualified_parts(qualified.as_str())?;
        let namespaces = Arc::make_mut(&mut self.namespaces);
        let previous = {
            let ns = namespaces
                .entry(namespace.into())
                .or_insert_with(|| Namespace::new(namespace));
            if ns.protected {
                return Err(EnvError::Protected(namespace.into()));
            }
            ns.data_families.shift_remove(name)
        };
        if let Some(previous) = previous {
            for binding in previous.generated_bindings {
                if let Ok((binding_namespace, binding_name)) = qualified_parts(binding.as_str())
                    && let Some(binding_ns) = namespaces.get_mut(binding_namespace)
                {
                    binding_ns.undefine(binding_name);
                }
            }
        }
        namespaces
            .get_mut(namespace)
            .expect("family namespace was just created")
            .data_families
            .insert(name.into(), family);
        Ok(())
    }

    pub fn is_data_family(&self, name: &str) -> bool {
        let Ok(qualified) = Self::qualify_data_family(name) else {
            return false;
        };
        let Ok((namespace, family)) = qualified_parts(qualified.as_str()) else {
            return false;
        };
        self.namespaces
            .get(namespace)
            .is_some_and(|ns| ns.data_families.contains_key(family))
    }

    /// Undefine one exact qualified data family and its recorded constructors.
    pub fn undefine_data_family(&mut self, family_name: &str) -> Result<(), EnvError> {
        let qualified = Self::qualify_data_family(family_name)?;
        let (namespace, name) = qualified_parts(qualified.as_str())?;
        let namespaces = Arc::make_mut(&mut self.namespaces);
        let ns = namespaces
            .get_mut(namespace)
            .ok_or_else(|| EnvError::FamilyNotFound(family_name.into()))?;
        if ns.protected {
            return Err(EnvError::Protected(namespace.into()));
        }
        let family = ns
            .data_families
            .shift_remove(name)
            .ok_or_else(|| EnvError::FamilyNotFound(family_name.into()))?;
        for binding in family.generated_bindings {
            if let Ok((binding_namespace, binding_name)) = qualified_parts(binding.as_str())
                && let Some(binding_ns) = namespaces.get_mut(binding_namespace)
            {
                binding_ns.undefine(binding_name);
            }
        }
        Ok(())
    }
}
