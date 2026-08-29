use crate::vm::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub next_version: u64,
    pub protected: bool,
    pub data_families: HashMap<String, DataFamily>,
    pub data_constructors: HashMap<String, (String, String, Vec<String>)>,
}

impl Namespace {
    pub fn new(name: &str) -> Self {
        let protected = name.starts_with("system/") || name == "kernel"
            || name == "inspect" || name.starts_with("control/");
        Namespace {
            bindings: HashMap::new(),
            history: HashMap::new(),
            next_version: 1,
            protected,
            data_families: HashMap::new(),
            data_constructors: HashMap::new(),
        }
    }

    pub fn define(&mut self, name: &str, value: Value) {
        if let Some(old) = self.bindings.get(name) {
            self.history.entry(name.to_string()).or_default().push(BindingRecord {
                value: old.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                version: self.next_version,
            });
            self.next_version += 1;
        }
        self.bindings.insert(name.to_string(), value);
    }

    pub fn undefine(&mut self, name: &str) -> Option<Value> {
        if self.protected {
            return None;
        }
        let removed = self.bindings.remove(name);
        if let Some(ref v) = removed {
            self.history.entry(name.to_string()).or_default().push(BindingRecord {
                value: v.clone(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                version: self.next_version,
            });
            self.next_version += 1;
        }
        removed
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.bindings.get(name)
    }

    pub fn history(&self, name: &str) -> Vec<&BindingRecord> {
        self.history.get(name).map(|h| h.iter().collect()).unwrap_or_default()
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
    pub namespaces: HashMap<String, Namespace>,
    pub frames: Vec<HashMap<String, Value>>,
    #[serde(skip)]
    pub serialized: String,
}

impl EnvRef {
    pub fn new() -> Self {
        let mut env = EnvRef {
            namespaces: HashMap::new(),
            frames: vec![HashMap::new()],
            serialized: String::new(),
        };
        env.namespaces.insert("system".into(), Namespace::new("system"));
        env.namespaces.insert("inspect".into(), Namespace::new("inspect"));
        env.namespaces.insert("control".into(), Namespace::new("control"));
        env.namespaces.insert("kernel".into(), Namespace::new("kernel"));
        env
    }

    pub fn lookup(&self, symbol: &str) -> Option<&Value> {
        if symbol.contains('/') {
            let parts: Vec<&str> = symbol.splitn(2, '/').collect();
            if let Some(ns) = self.namespaces.get(parts[0]) {
                return ns.get(parts[1]);
            }
            return None;
        }

        for frame in self.frames.iter().rev() {
            if let Some(v) = frame.get(symbol) {
                return Some(v);
            }
        }

        if let Some(ns) = self.namespaces.get("user") {
            if let Some(v) = ns.get(symbol) {
                return Some(v);
            }
        }

        if let Some(ns) = self.namespaces.get("kernel") {
            if let Some(v) = ns.get(symbol) {
                return Some(v);
            }
        }

        None
    }

    pub fn define(&mut self, qualified_name: &str, value: Value) -> Result<(), String> {
        let parts: Vec<&str> = qualified_name.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(format!("define requires a qualified name (namespace/name), got '{}'", qualified_name));
        }

        let ns_name = parts[0].to_string();
        let name = parts[1].to_string();

        let ns = self.namespaces.entry(ns_name.clone()).or_insert_with(|| Namespace::new(&ns_name));
        if ns.protected {
            return Err(format!("namespace '{}' is protected and cannot be modified", ns_name));
        }

        ns.define(&name, value);
        Ok(())
    }

    /// Force define, bypassing protection checks (for kernel use only).
    pub fn force_define(&mut self, qualified_name: &str, value: Value) {
        let parts: Vec<&str> = qualified_name.splitn(2, '/').collect();
        if parts.len() != 2 {
            return;
        }
        let ns_name = parts[0].to_string();
        let name = parts[1].to_string();
        let ns = self.namespaces.entry(ns_name.clone()).or_insert_with(|| Namespace::new(&ns_name));
        let _ = ns.protected;
        ns.define(&name, value);
    }

    pub fn undefine(&mut self, qualified_name: &str) -> Result<(), String> {
        let parts: Vec<&str> = qualified_name.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(format!("undefine requires a qualified name (namespace/name), got '{}'", qualified_name));
        }

        if let Some(ns) = self.namespaces.get_mut(parts[0]) {
            ns.undefine(parts[1]);
            Ok(())
        } else {
            Err(format!("namespace '{}' not found", parts[0]))
        }
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

    pub fn set_data_family(&mut self, family_name: &str, family: DataFamily) {
        let parts: Vec<&str> = family_name.splitn(2, '/').collect();
        let name = if parts.len() == 2 { parts[1].to_string() } else { family_name.to_string() };
        let ns_name = if parts.len() == 2 { parts[0].to_string() } else { "kernel".to_string() };
        let ns = self.namespaces.entry(ns_name).or_insert_with(|| Namespace::new("kernel"));
        ns.data_families.insert(name, family);
    }

    pub fn set_data_constructor(&mut self, qualified_name: &str, family: String, variant: String, fields: Vec<String>) {
        let parts: Vec<&str> = qualified_name.splitn(2, '/').collect();
        if parts.len() != 2 { return; }
        let ns_name = parts[0];
        let name = parts[1];
        let ns = self.namespaces.entry(ns_name.to_string()).or_insert_with(|| Namespace::new(ns_name));
        ns.data_constructors.insert(name.to_string(), (family, variant, fields));
    }

    pub fn serialize_env_for_closure(&mut self) {
        self.serialized = serde_json::to_string(self).unwrap_or_default();
    }
}
