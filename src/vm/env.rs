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
    pub name: String,
    pub variants: Vec<DataVariant>,
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

/// A reference to the root environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvRef {
    pub namespaces: Arc<HashMap<String, Namespace>>,
    pub frames: Vec<HashMap<String, Value>>,
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
            frames: vec![HashMap::new()],
        }
    }

    pub fn lookup(&self, symbol: &str) -> Option<&Value> {
        if let Some((namespace, name)) = symbol.split_once('/') {
            return self.namespaces.get(namespace)?.get(name);
        }
        self.frames
            .iter()
            .rev()
            .find_map(|frame| frame.get(symbol))
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

    pub fn push_frame(&mut self) {
        self.frames.push(HashMap::new());
    }

    pub fn pop_frame(&mut self) {
        self.frames.pop();
    }

    pub fn set_lexical(&mut self, name: &str, value: Value) {
        if let Some(frame) = self.frames.last_mut() {
            frame.insert(name.to_string(), value);
        }
    }

    pub fn namespace_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.namespaces.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn set_data_family(&mut self, family_name: &str, family: DataFamily) -> Result<(), String> {
        let (namespace, name) = family_name.split_once('/').unwrap_or(("user", family_name));
        let ns = Arc::make_mut(&mut self.namespaces)
            .entry(namespace.into())
            .or_insert_with(|| Namespace::new(namespace));
        if ns.protected {
            return Err(format!(
                "namespace '{}' is protected and cannot be modified",
                namespace
            ));
        }
        ns.data_families.insert(name.into(), family);
        Ok(())
    }

    pub fn is_data_family(&self, name: &str) -> bool {
        // Check if any namespace has a data family matching this name
        // name can be "Foo" or "my/Foo"
        for ns in self.namespaces.values() {
            for fam_name in ns.data_families.keys() {
                if fam_name == name || name.ends_with(fam_name) || fam_name.ends_with(name) {
                    return true;
                }
            }
        }
        false
    }

    /// Undefine a data family, removing all its constructors atomically.
    /// Undefine a data family, removing all its constructors atomically.
    pub fn undefine_data_family(&mut self, family_name: &str) -> Result<(), String> {
        let leaf = family_name
            .split('/')
            .next_back()
            .unwrap_or(family_name)
            .to_string();

        // Check if this family exists in any namespace
        let exists = self
            .namespaces
            .values()
            .any(|ns| ns.data_families.contains_key(&leaf));
        if !exists {
            return Err(format!("data family '{}' not found", family_name));
        }

        // Remove constructors from user namespace by prefix
        if let Some(user) = Arc::make_mut(&mut self.namespaces).get_mut("user") {
            let prefix = format!("{}/", leaf);
            user.bindings.retain(|k, _| !k.starts_with(&prefix));
        }

        // Remove the family metadata from all namespaces
        for ns in Arc::make_mut(&mut self.namespaces).values_mut() {
            ns.data_families.retain(|k, _| k != &leaf);
        }

        Ok(())
    }
}
