use crate::ids::QualifiedName;
use crate::state::{BoundedLog, Stamped};
use crate::vm::value::{Value, collect_captured_environments};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;

const HISTORY_LIMIT: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("invalid qualified name: {0}")]
    InvalidName(String),
    #[error("immutable binding '{0}' cannot be modified")]
    ImmutableBinding(String),
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum BindingOrigin {
    Kernel,
    Prelude,
    #[default]
    Agent,
}

impl BindingOrigin {
    fn mutable(self) -> bool {
        self == Self::Agent
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum BindingChange {
    Defined {
        source: Option<String>,
        preview: String,
    },
    Undefined,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BindingVersion {
    pub version: u64,
    pub change: BindingChange,
}

pub type BindingRecord = Stamped<BindingVersion>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataVariant {
    pub name: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataFamily {
    pub name: QualifiedName,
    pub variants: Vec<DataVariant>,
    pub generated_bindings: Vec<QualifiedName>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Namespace {
    pub(crate) bindings: IndexMap<String, Value>,
    pub(crate) history: IndexMap<String, BoundedLog<BindingRecord, HISTORY_LIMIT>>,
    pub(crate) sources: IndexMap<String, String>,
    pub(crate) origins: IndexMap<String, BindingOrigin>,
    pub(crate) next_version: u64,
    pub(crate) data_families: IndexMap<String, DataFamily>,
}

impl Default for Namespace {
    fn default() -> Self {
        Self {
            bindings: IndexMap::new(),
            history: IndexMap::new(),
            sources: IndexMap::new(),
            origins: IndexMap::new(),
            next_version: 1,
            data_families: IndexMap::new(),
        }
    }
}

impl Namespace {
    pub fn new() -> Self {
        Self::default()
    }

    fn record(&mut self, name: &str, change: BindingChange) {
        let version = self.next_version;
        self.next_version = self.next_version.saturating_add(1);
        self.history
            .entry(name.into())
            .or_default()
            .push(Stamped::now(BindingVersion { version, change }));
    }

    fn define(
        &mut self,
        name: &str,
        value: Value,
        source: Option<String>,
        origin: BindingOrigin,
        record: bool,
    ) {
        if record {
            self.record(
                name,
                BindingChange::Defined {
                    source: source.clone(),
                    preview: value.to_string().chars().take(512).collect(),
                },
            );
        }
        self.bindings.insert(name.into(), value);
        self.origins.insert(name.into(), origin);
        match source {
            Some(source) => {
                self.sources.insert(name.into(), source);
            }
            None => {
                self.sources.shift_remove(name);
            }
        }
    }

    fn assign(&mut self, name: &str, value: Value) -> bool {
        if !self.bindings.contains_key(name) {
            return false;
        }
        self.bindings.insert(name.into(), value);
        true
    }

    fn undefine(&mut self, name: &str) -> Option<Value> {
        let removed = self.bindings.shift_remove(name)?;
        self.sources.shift_remove(name);
        self.origins.shift_remove(name);
        self.record(name, BindingChange::Undefined);
        Some(removed)
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.bindings.get(name)
    }

    pub fn history(&self, name: &str) -> Option<&BoundedLog<BindingRecord, HISTORY_LIMIT>> {
        self.history.get(name)
    }

    pub fn list_bindings(&self) -> Vec<String> {
        let mut names: Vec<_> = self.bindings.keys().cloned().collect();
        names.sort();
        names
    }
}

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

#[derive(Debug)]
struct BindingUndo {
    environment: EnvironmentId,
    name: String,
    previous: Option<CellId>,
}

#[derive(Debug)]
struct LexicalTransaction {
    next_environment_id: EnvironmentId,
    next_cell_id: CellId,
    changed_cells: IndexMap<CellId, Value>,
    changed_bindings: Vec<BindingUndo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LexicalArena {
    pub(crate) environments: IndexMap<EnvironmentId, LexicalEnvironment>,
    pub(crate) cells: IndexMap<CellId, Value>,
    next_environment_id: EnvironmentId,
    next_cell_id: CellId,
    #[serde(skip)]
    transaction: Option<LexicalTransaction>,
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
            transaction: None,
        }
    }
}

impl LexicalArena {
    fn begin_transaction(&mut self) {
        assert!(self.transaction.is_none(), "nested lexical transaction");
        self.transaction = Some(LexicalTransaction {
            next_environment_id: self.next_environment_id,
            next_cell_id: self.next_cell_id,
            changed_cells: IndexMap::new(),
            changed_bindings: Vec::new(),
        });
    }

    fn commit_transaction(&mut self) {
        self.transaction = None;
    }

    fn rollback_transaction(&mut self) {
        let Some(transaction) = self.transaction.take() else {
            return;
        };
        for undo in transaction.changed_bindings.into_iter().rev() {
            let bindings = &mut self
                .environments
                .get_mut(&undo.environment)
                .expect("transaction environment must exist")
                .bindings;
            match undo.previous {
                Some(cell) => {
                    bindings.insert(undo.name, cell);
                }
                None => {
                    bindings.shift_remove(&undo.name);
                }
            }
        }
        self.environments
            .retain(|id, _| id.0 < transaction.next_environment_id.0);
        self.cells.retain(|id, _| id.0 < transaction.next_cell_id.0);
        for (cell, value) in transaction.changed_cells {
            self.cells.insert(cell, value);
        }
        self.next_environment_id = transaction.next_environment_id;
        self.next_cell_id = transaction.next_cell_id;
    }

    fn write_cell(&mut self, cell: CellId, value: Value) {
        if let Some(transaction) = &mut self.transaction
            && cell.0 < transaction.next_cell_id.0
            && !transaction.changed_cells.contains_key(&cell)
            && let Some(previous) = self.cells.get(&cell)
        {
            transaction.changed_cells.insert(cell, previous.clone());
        }
        self.cells.insert(cell, value);
    }

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
        if let Some(cell) = self
            .environments
            .get(&environment)
            .and_then(|frame| frame.bindings.get(name))
            .copied()
        {
            self.write_cell(cell, value);
            return;
        }
        let previous = self
            .environments
            .get(&environment)
            .expect("active lexical environment must exist")
            .bindings
            .get(name)
            .copied();
        if let Some(transaction) = &mut self.transaction
            && environment.0 < transaction.next_environment_id.0
            && !transaction
                .changed_bindings
                .iter()
                .any(|undo| undo.environment == environment && undo.name == name)
        {
            transaction.changed_bindings.push(BindingUndo {
                environment,
                name: name.into(),
                previous,
            });
        }
        let cell = self.next_cell_id.take_and_advance();
        self.cells.insert(cell, value);
        self.environments
            .get_mut(&environment)
            .expect("active lexical environment must exist")
            .bindings
            .insert(name.into(), cell);
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

#[derive(Debug)]
pub(crate) struct EnvTransaction {
    namespaces: Arc<IndexMap<String, Namespace>>,
    current_environment: EnvironmentId,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EnvRef {
    pub(crate) namespaces: Arc<IndexMap<String, Namespace>>,
    pub(crate) lexical: LexicalArena,
    current_environment: EnvironmentId,
    #[serde(skip)]
    transaction: Option<EnvTransaction>,
}

impl Default for EnvRef {
    fn default() -> Self {
        Self::new()
    }
}

impl EnvRef {
    pub(crate) fn begin_transaction(&mut self) {
        assert!(self.transaction.is_none(), "nested environment transaction");
        self.transaction = Some(EnvTransaction {
            namespaces: self.namespaces.clone(),
            current_environment: self.current_environment,
        });
        self.lexical.begin_transaction();
    }

    pub(crate) fn commit_transaction(&mut self) {
        self.lexical.commit_transaction();
        self.transaction = None;
    }

    pub(crate) fn rollback_transaction(&mut self) {
        self.lexical.rollback_transaction();
        if let Some(transaction) = self.transaction.take() {
            self.namespaces = transaction.namespaces;
            self.current_environment = transaction.current_environment;
        }
    }

    pub fn new() -> Self {
        let namespaces = ["system", "inspect", "control", "kernel", "user"]
            .into_iter()
            .map(|name| (name.into(), Namespace::new()))
            .collect();
        Self {
            namespaces: Arc::new(namespaces),
            lexical: LexicalArena::default(),
            current_environment: EnvironmentId::ROOT,
            transaction: None,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        self.lexical.validate(self.current_environment)?;
        for (namespace_name, namespace) in self.namespaces.iter() {
            if namespace_name.is_empty()
                || namespace_name.contains('/')
                || namespace.next_version == 0
                || namespace.next_version == u64::MAX
                || namespace.bindings.keys().any(String::is_empty)
                || namespace
                    .bindings
                    .keys()
                    .any(|name| !namespace.origins.contains_key(name))
                || namespace
                    .origins
                    .keys()
                    .any(|name| !namespace.bindings.contains_key(name))
            {
                return Err(format!(
                    "namespace '{namespace_name}' has invalid serialized state"
                ));
            }
        }
        let mut captured = HashSet::new();
        for namespace in self.namespaces.values() {
            collect_captured_environments(namespace.bindings.values(), &mut captured);
        }
        collect_captured_environments(self.lexical.cells.values(), &mut captured);
        if let Some(missing) = captured
            .into_iter()
            .find(|id| !self.lexical.environments.contains_key(id))
        {
            return Err(format!(
                "captured lexical environment {missing} does not exist"
            ));
        }
        Ok(())
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
                ["user", "kernel"]
                    .into_iter()
                    .find_map(|namespace| self.namespaces.get(namespace)?.get(symbol))
            })
    }

    pub fn define(
        &mut self,
        qualified_name: &str,
        value: Value,
        source: Option<String>,
        origin: BindingOrigin,
    ) -> Result<(), EnvError> {
        let (namespace, name) = qualified_parts(qualified_name)?;
        let namespaces = Arc::make_mut(&mut self.namespaces);
        let ns = namespaces.entry(namespace.into()).or_default();
        if ns.origins.get(name).is_some_and(|origin| !origin.mutable()) {
            return Err(EnvError::ImmutableBinding(qualified_name.into()));
        }
        ns.define(name, value, source, origin, true);
        Ok(())
    }

    pub(crate) fn force_define(
        &mut self,
        qualified_name: &str,
        value: Value,
        source: Option<String>,
        origin: BindingOrigin,
    ) {
        let Ok((namespace, name)) = qualified_parts(qualified_name) else {
            return;
        };
        Arc::make_mut(&mut self.namespaces)
            .entry(namespace.into())
            .or_default()
            .define(name, value, source, origin, false);
    }

    pub fn assign(&mut self, qualified_name: &str, value: Value) -> Result<(), EnvError> {
        let (namespace, name) = qualified_parts(qualified_name)?;
        let ns = Arc::make_mut(&mut self.namespaces)
            .get_mut(namespace)
            .ok_or_else(|| EnvError::NamespaceNotFound(namespace.into()))?;
        if ns.origins.get(name).is_some_and(|origin| !origin.mutable()) {
            return Err(EnvError::ImmutableBinding(qualified_name.into()));
        }
        ns.assign(name, value)
            .then_some(())
            .ok_or_else(|| EnvError::BindingNotFound(qualified_name.into()))
    }

    pub fn source(&self, qualified_name: &str) -> Option<&str> {
        let (namespace, name) = qualified_parts(qualified_name).ok()?;
        self.namespaces
            .get(namespace)?
            .sources
            .get(name)
            .map(String::as_str)
    }

    pub fn undefine(&mut self, qualified_name: &str) -> Result<(), EnvError> {
        let (namespace, name) = qualified_parts(qualified_name)?;
        let ns = Arc::make_mut(&mut self.namespaces)
            .get_mut(namespace)
            .ok_or_else(|| EnvError::NamespaceNotFound(namespace.into()))?;
        if ns.origins.get(name).is_some_and(|origin| !origin.mutable()) {
            return Err(EnvError::ImmutableBinding(qualified_name.into()));
        }
        ns.undefine(name)
            .map(drop)
            .ok_or_else(|| EnvError::BindingNotFound(qualified_name.into()))
    }

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
        self.lexical.write_cell(cell, value);
        true
    }

    pub fn lexical_arena_counts(&self) -> (usize, usize) {
        (self.lexical.environments.len(), self.lexical.cells.len())
    }

    pub fn binding_history_len(&self, namespace: &str, name: &str) -> usize {
        self.namespaces
            .get(namespace)
            .and_then(|namespace| namespace.history(name))
            .map_or(0, BoundedLog::len)
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

    pub(crate) fn push_call_frame(&mut self, captured: EnvironmentId) -> Result<(), EnvError> {
        if !self.lexical.environments.contains_key(&captured) {
            return Err(EnvError::MissingEnvironment(captured));
        }
        self.current_environment = self.lexical.allocate_environment(captured);
        Ok(())
    }

    pub fn namespace_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.namespaces.keys().cloned().collect();
        names.sort();
        names
    }

    fn qualify_data_family(family_name: &str) -> Result<QualifiedName, EnvError> {
        let name = if family_name.contains('/') {
            family_name.into()
        } else {
            format!("user/{family_name}")
        };
        qualified_parts(&name)?;
        Ok(QualifiedName::new(name))
    }

    pub fn set_data_family(&mut self, mut family: DataFamily) -> Result<(), EnvError> {
        let qualified = Self::qualify_data_family(family.name.as_str())?;
        family.name = qualified.clone();
        let (namespace, name) = qualified_parts(qualified.as_str())?;
        let namespaces = Arc::make_mut(&mut self.namespaces);
        let ns = namespaces.entry(namespace.into()).or_default();
        if let Some(previous) = ns.data_families.shift_remove(name) {
            for binding in previous.generated_bindings {
                if let Ok((binding_namespace, binding_name)) = qualified_parts(binding.as_str())
                    && let Some(namespace) = namespaces.get_mut(binding_namespace)
                {
                    namespace.undefine(binding_name);
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
            .is_some_and(|namespace| namespace.data_families.contains_key(family))
    }

    pub fn undefine_data_family(&mut self, family_name: &str) -> Result<(), EnvError> {
        let qualified = Self::qualify_data_family(family_name)?;
        let (namespace, name) = qualified_parts(qualified.as_str())?;
        let namespaces = Arc::make_mut(&mut self.namespaces);
        let family = namespaces
            .get_mut(namespace)
            .and_then(|namespace| namespace.data_families.shift_remove(name))
            .ok_or_else(|| EnvError::FamilyNotFound(family_name.into()))?;
        for binding in family.generated_bindings {
            if let Ok((binding_namespace, binding_name)) = qualified_parts(binding.as_str())
                && let Some(namespace) = namespaces.get_mut(binding_namespace)
            {
                namespace.undefine(binding_name);
            }
        }
        Ok(())
    }
}
