use crate::vm::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

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
    pub name: String,
    pub variants: Vec<DataVariant>,
    /// Exact qualified bindings installed for this definition.
    #[serde(default)]
    pub generated_bindings: Vec<String>,
}

/// A single namespace holding named bindings with version history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Namespace {
    pub bindings: HashMap<String, Value>,
    pub history: HashMap<String, Vec<BindingRecord>>,
    #[serde(default)]
    pub sources: HashMap<String, String>,
    pub next_version: u64,
    pub protected: bool,
    pub data_families: HashMap<String, DataFamily>,
}

impl Namespace {
    pub fn new(name: &str) -> Self {
        let protected =
            name == "system" || name == "kernel" || name == "inspect" || name == "control";
        Namespace {
            bindings: HashMap::new(),
            history: HashMap::new(),
            sources: HashMap::new(),
            next_version: 1,
            protected,
            data_families: HashMap::new(),
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
        let removed = self.bindings.remove(name);
        self.sources.remove(name);
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
pub type EnvironmentId = u64;
pub type CellId = u64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LexicalEnvironment {
    pub parent: Option<EnvironmentId>,
    pub bindings: HashMap<String, CellId>,
}

/// Serializable storage for lexical scopes and their mutable binding cells.
///
/// Environments point at cells rather than containing values directly. A
/// closure therefore captures an environment id, and all closures which see a
/// binding share the same cell (including after snapshot recovery).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LexicalArena {
    pub environments: HashMap<EnvironmentId, LexicalEnvironment>,
    pub cells: HashMap<CellId, Value>,
    next_environment_id: EnvironmentId,
    next_cell_id: CellId,
}

impl Default for LexicalArena {
    fn default() -> Self {
        let mut environments = HashMap::new();
        environments.insert(
            0,
            LexicalEnvironment {
                parent: None,
                bindings: HashMap::new(),
            },
        );
        Self {
            environments,
            cells: HashMap::new(),
            next_environment_id: 1,
            next_cell_id: 1,
        }
    }
}

impl LexicalArena {
    fn allocate_environment(&mut self, parent: EnvironmentId) -> EnvironmentId {
        let id = self.next_environment_id;
        self.next_environment_id += 1;
        self.environments.insert(
            id,
            LexicalEnvironment {
                parent: Some(parent),
                bindings: HashMap::new(),
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

        let cell = self.next_cell_id;
        self.next_cell_id += 1;
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
    pub namespaces: Arc<HashMap<String, Namespace>>,
    #[serde(default)]
    pub lexical: LexicalArena,
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
        let mut namespaces = HashMap::new();
        namespaces.insert("system".into(), Namespace::new("system"));
        namespaces.insert("inspect".into(), Namespace::new("inspect"));
        namespaces.insert("control".into(), Namespace::new("control"));
        namespaces.insert("kernel".into(), Namespace::new("kernel"));
        EnvRef {
            namespaces: Arc::new(namespaces),
            lexical: LexicalArena::default(),
            current_environment: 0,
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

    pub fn define(&mut self, qualified_name: &str, value: Value) -> Result<(), String> {
        let parts: Vec<&str> = qualified_name.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(format!(
                "define requires a qualified name (namespace/name), got '{}'",
                qualified_name
            ));
        }

        let ns_name = parts[0].to_string();
        let name = parts[1].to_string();

        let ns = Arc::make_mut(&mut self.namespaces)
            .entry(ns_name.clone())
            .or_insert_with(|| Namespace::new(&ns_name));
        if ns.protected {
            return Err(format!(
                "namespace '{}' is protected and cannot be modified",
                ns_name
            ));
        }

        ns.define(&name, value);
        Ok(())
    }

    pub fn store_source(&mut self, qualified_name: &str, source: String) -> Result<(), String> {
        let (namespace, name) = qualified_name
            .split_once('/')
            .ok_or_else(|| "source name must be qualified".to_string())?;
        Arc::make_mut(&mut self.namespaces)
            .get_mut(namespace)
            .ok_or_else(|| format!("namespace '{}' not found", namespace))?
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
    pub fn force_define(&mut self, qualified_name: &str, value: Value) {
        let Some((namespace, name)) = qualified_name.split_once('/') else {
            return;
        };
        Arc::make_mut(&mut self.namespaces)
            .entry(namespace.into())
            .or_insert_with(|| Namespace::new(namespace))
            .bindings
            .insert(name.into(), value);
    }

    pub fn undefine(&mut self, qualified_name: &str) -> Result<(), String> {
        let Some((namespace, name)) = qualified_name.split_once('/') else {
            return Err(format!(
                "undefine requires namespace/name, got '{}'",
                qualified_name
            ));
        };
        let ns = Arc::make_mut(&mut self.namespaces)
            .get_mut(namespace)
            .ok_or_else(|| format!("namespace '{}' not found", namespace))?;
        if ns.protected {
            return Err(format!(
                "namespace '{}' is protected and cannot be modified",
                namespace
            ));
        }
        ns.undefine(name)
            .ok_or_else(|| format!("binding '{}' not found", qualified_name))?;
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

    pub fn current_environment(&self) -> EnvironmentId {
        self.current_environment
    }

    pub fn activate_environment(&mut self, environment: EnvironmentId) -> Result<(), String> {
        if !self.lexical.environments.contains_key(&environment) {
            return Err(format!("closure environment {} is missing", environment));
        }
        self.current_environment = environment;
        Ok(())
    }

    /// Allocate a call frame parented directly to a closure's captured scope.
    pub fn push_call_frame(&mut self, captured: EnvironmentId) -> Result<(), String> {
        if !self.lexical.environments.contains_key(&captured) {
            return Err(format!("closure environment {} is missing", captured));
        }
        self.current_environment = self.lexical.allocate_environment(captured);
        Ok(())
    }

    pub fn namespace_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.namespaces.keys().cloned().collect();
        names.sort();
        names
    }

    fn qualify_data_family(family_name: &str) -> String {
        if family_name.contains('/') {
            family_name.to_string()
        } else {
            format!("user/{}", family_name)
        }
    }

    /// Replace family metadata and remove only constructors generated by the
    /// previous definition of this exact qualified family.
    pub fn set_data_family(&mut self, mut family: DataFamily) -> Result<(), String> {
        let qualified = Self::qualify_data_family(&family.name);
        family.name = qualified.clone();
        let (namespace, name) = qualified
            .split_once('/')
            .expect("qualified family always contains a namespace");
        let namespaces = Arc::make_mut(&mut self.namespaces);
        let previous = {
            let ns = namespaces
                .entry(namespace.into())
                .or_insert_with(|| Namespace::new(namespace));
            if ns.protected {
                return Err(format!(
                    "namespace '{}' is protected and cannot be modified",
                    namespace
                ));
            }
            ns.data_families.remove(name)
        };
        if let Some(previous) = previous {
            for binding in previous.generated_bindings {
                if let Some((binding_namespace, binding_name)) = binding.split_once('/')
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
        let Some((namespace, family)) = qualified.split_once('/') else {
            return false;
        };
        self.namespaces
            .get(namespace)
            .is_some_and(|ns| ns.data_families.contains_key(family))
    }

    /// Undefine one exact qualified data family and its recorded constructors.
    pub fn undefine_data_family(&mut self, family_name: &str) -> Result<(), String> {
        let qualified = Self::qualify_data_family(family_name);
        let (namespace, name) = qualified
            .split_once('/')
            .expect("qualified family always contains a namespace");
        let namespaces = Arc::make_mut(&mut self.namespaces);
        let ns = namespaces
            .get_mut(namespace)
            .ok_or_else(|| format!("data family '{}' not found", family_name))?;
        if ns.protected {
            return Err(format!(
                "namespace '{}' is protected and cannot be modified",
                namespace
            ));
        }
        let family = ns
            .data_families
            .remove(name)
            .ok_or_else(|| format!("data family '{}' not found", family_name))?;
        for binding in family.generated_bindings {
            if let Some((binding_namespace, binding_name)) = binding.split_once('/')
                && let Some(binding_ns) = namespaces.get_mut(binding_namespace)
            {
                binding_ns.undefine(binding_name);
            }
        }
        Ok(())
    }
}
