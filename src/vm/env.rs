use crate::ids::QualifiedName;
use crate::vm::value::Value;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("invalid qualified name: {0}")]
    InvalidName(String),
    #[error("namespace '{0}' is protected and cannot be modified")]
    Protected(String),
    #[error("namespace '{0}' not found")]
    NamespaceNotFound(String),
    #[error("binding '{0}' not found")]
    BindingNotFound(String),
    #[error("closure environment {0} is missing")]
    MissingEnvironment(EnvironmentId),
    #[error("data family '{0}' not found")]
    FamilyNotFound(String),
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

    pub fn history(&self, name: &str) -> Vec<&BindingRecord> {
        self.history
            .get(name)
            .map(|h| h.iter().collect())
            .unwrap_or_default()
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvRef {
    pub(crate) namespaces: Arc<IndexMap<String, Namespace>>,
    #[serde(default)]
    pub(crate) lexical: LexicalArena,
    #[serde(default)]
    current_environment: EnvironmentId,
}

impl Default for EnvRef {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvRef {
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
        if let Some((namespace, name)) = symbol.split_once('/') {
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
        let parts: Vec<&str> = qualified_name.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(EnvError::InvalidName(qualified_name.into()));
        }

        let ns_name = parts[0].to_string();
        let name = parts[1].to_string();

        let ns = Arc::make_mut(&mut self.namespaces)
            .entry(ns_name.clone())
            .or_insert_with(|| Namespace::new(&ns_name));
        if ns.protected {
            return Err(EnvError::Protected(ns_name));
        }

        ns.define(&name, value);
        Ok(())
    }

    pub fn store_source(&mut self, qualified_name: &str, source: String) -> Result<(), EnvError> {
        let (namespace, name) = qualified_name
            .split_once('/')
            .ok_or_else(|| EnvError::InvalidName(qualified_name.into()))?;
        Arc::make_mut(&mut self.namespaces)
            .get_mut(namespace)
            .ok_or_else(|| EnvError::NamespaceNotFound(namespace.into()))?
            .sources
            .insert(name.into(), source);
        Ok(())
    }

    pub fn source(&self, qualified_name: &str) -> Option<&str> {
        let (namespace, name) = qualified_name.split_once('/')?;
        self.namespaces
            .get(namespace)?
            .sources
            .get(name)
            .map(String::as_str)
    }

    /// Force define, bypassing protection checks (for kernel use only).
    pub(crate) fn force_define(&mut self, qualified_name: &str, value: Value) {
        let Some((namespace, name)) = qualified_name.split_once('/') else {
            return;
        };
        Arc::make_mut(&mut self.namespaces)
            .entry(namespace.into())
            .or_insert_with(|| Namespace::new(namespace))
            .bindings
            .insert(name.into(), value);
    }

    pub fn undefine(&mut self, qualified_name: &str) -> Result<(), EnvError> {
        let Some((namespace, name)) = qualified_name.split_once('/') else {
            return Err(EnvError::InvalidName(qualified_name.into()));
        };
        let ns = Arc::make_mut(&mut self.namespaces)
            .get_mut(namespace)
            .ok_or_else(|| EnvError::NamespaceNotFound(namespace.into()))?;
        if ns.protected {
            return Err(EnvError::Protected(namespace.into()));
        }
        ns.undefine(name)
            .ok_or_else(|| EnvError::BindingNotFound(qualified_name.into()))?;
        Ok(())
    }

    /// Allocate a lexical child of the currently active environment.
    pub fn push_frame(&mut self) {
        self.current_environment = self.lexical.allocate_environment(self.current_environment);
    }

    /// Restore the parent of the currently active lexical environment.
    pub fn pop_frame(&mut self) {
        if let Some(parent) = self
            .lexical
            .environments
            .get(&self.current_environment)
            .and_then(|environment| environment.parent)
        {
            self.current_environment = parent;
        }
    }

    pub fn set_lexical(&mut self, name: &str, value: Value) {
        self.lexical.define(self.current_environment, name, value);
    }

    pub fn set_existing_lexical(&mut self, name: &str, value: Value) -> bool {
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

    pub fn current_environment(&self) -> EnvironmentId {
        self.current_environment
    }

    pub fn activate_environment(&mut self, environment: EnvironmentId) -> Result<(), EnvError> {
        if !self.lexical.environments.contains_key(&environment) {
            return Err(EnvError::MissingEnvironment(environment));
        }
        self.current_environment = environment;
        Ok(())
    }

    /// Allocate a call frame parented directly to a closure's captured scope.
    pub fn push_call_frame(&mut self, captured: EnvironmentId) -> Result<(), EnvError> {
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

    fn qualify_data_family(family_name: &str) -> QualifiedName {
        QualifiedName::new(if family_name.contains('/') {
            family_name.to_string()
        } else {
            format!("user/{}", family_name)
        })
    }

    /// Replace family metadata and remove only constructors generated by the
    /// previous definition of this exact qualified family.
    pub fn set_data_family(&mut self, mut family: DataFamily) -> Result<(), EnvError> {
        let qualified = Self::qualify_data_family(family.name.as_str());
        family.name = qualified.clone();
        let (namespace, name) = qualified
            .as_str()
            .split_once('/')
            .expect("qualified family always contains a namespace");
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
                if let Some((binding_namespace, binding_name)) = binding.as_str().split_once('/')
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
        let qualified = Self::qualify_data_family(name);
        let Some((namespace, family)) = qualified.as_str().split_once('/') else {
            return false;
        };
        self.namespaces
            .get(namespace)
            .is_some_and(|ns| ns.data_families.contains_key(family))
    }

    /// Undefine one exact qualified data family and its recorded constructors.
    pub fn undefine_data_family(&mut self, family_name: &str) -> Result<(), EnvError> {
        let qualified = Self::qualify_data_family(family_name);
        let (namespace, name) = qualified
            .as_str()
            .split_once('/')
            .expect("qualified family always contains a namespace");
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
            if let Some((binding_namespace, binding_name)) = binding.as_str().split_once('/')
                && let Some(binding_ns) = namespaces.get_mut(binding_namespace)
            {
                binding_ns.undefine(binding_name);
            }
        }
        Ok(())
    }
}
