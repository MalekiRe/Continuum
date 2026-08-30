use crate::kernel::Kernel;
use crate::kernel::native::{arithmetic, exact_native, numbers};
use crate::vm::value::{Arity, NativeError, Value};

impl Kernel {
    pub(crate) fn register_vm_primitives(&mut self) {
        exact_native!(self, "kernel/+", |_kernel, [left, right]| {
            arithmetic(left, right, "+", i64::checked_add, |a, b| a + b)
        });
        exact_native!(self, "kernel/-", |_kernel, [left, right]| {
            arithmetic(left, right, "-", i64::checked_sub, |a, b| a - b)
        });
        exact_native!(self, "kernel/*", |_kernel, [left, right]| {
            arithmetic(left, right, "*", i64::checked_mul, |a, b| a * b)
        });
        exact_native!(self, "kernel//", |_kernel, [left, right]| {
            let (a, b) = numbers(left, right, "/")?;
            if b == 0.0 {
                Err("/: division by zero".into())
            } else {
                Ok(Value::Float(a / b))
            }
        });
        exact_native!(self, "kernel/=", |_kernel, [left, right]| Ok(Value::Bool(
            left == right
        )));
        exact_native!(self, "kernel/<", |_kernel, [left, right]| {
            let (a, b) = numbers(left, right, "<")?;
            Ok(Value::Bool(a < b))
        });
        exact_native!(self, "kernel/>", |_kernel, [left, right]| {
            let (a, b) = numbers(left, right, ">")?;
            Ok(Value::Bool(a > b))
        });
        exact_native!(self, "kernel/cons", |_kernel, [car, cdr]| match cdr {
            Value::List(items) => Ok(Value::List(
                std::iter::once((*car).clone())
                    .chain(items.iter().cloned())
                    .collect(),
            )),
            Value::Nil => Ok(Value::List(vec![(*car).clone()])),
            _ => Err("cons: second argument must be a list".into()),
        });
        exact_native!(self, "kernel/car", |_kernel, [value]| match value {
            Value::List(items) => items
                .first()
                .cloned()
                .ok_or_else(|| "car: empty list".into()),
            _ => Err("car: expected list".into()),
        });
        exact_native!(self, "kernel/cdr", |_kernel, [value]| match value {
            Value::List(items) if items.len() >= 2 => Ok(Value::List(items[1..].to_vec())),
            Value::List(_) => Ok(Value::Nil),
            _ => Err("cdr: expected list".into()),
        });
        self.define_native("kernel/list", Arity::Variadic, |_kernel, args| {
            Ok(Value::List(args))
        });
        exact_native!(self, "kernel/display", |kernel, [value]| {
            kernel.write_output(&value.to_string());
            Ok((*value).clone())
        });
        exact_native!(self, "kernel/println", |kernel, [value]| {
            kernel.write_output(&format!("{value}\n"));
            Ok((*value).clone())
        });
        exact_native!(self, "kernel/nil?", |_kernel, [value]| Ok(Value::Bool(
            matches!(value, Value::Nil)
        )));
        exact_native!(self, "kernel/number?", |_kernel, [value]| {
            Ok(Value::Bool(matches!(
                value,
                Value::Int(_) | Value::Float(_)
            )))
        });
        exact_native!(self, "kernel/symbol?", |_kernel, [value]| Ok(Value::Bool(
            matches!(value, Value::Symbol(_))
        )));
        exact_native!(self, "kernel/string?", |_kernel, [value]| Ok(Value::Bool(
            matches!(value, Value::String(_))
        )));
        exact_native!(self, "kernel/list?", |_kernel, [value]| Ok(Value::Bool(
            value.is_list()
        )));
        exact_native!(self, "kernel/function?", |_kernel, [value]| Ok(
            Value::Bool(matches!(value, Value::Function(_)))
        ));
        exact_native!(self, "kernel/keyword?", |_kernel, [value]| Ok(Value::Bool(
            matches!(value, Value::Keyword(_))
        )));
        exact_native!(self, "string-append", |_kernel, [left, right]| {
            Ok(Value::string(&format!(
                "{}{}",
                left.coerce_text(),
                right.coerce_text()
            )))
        });
        exact_native!(self, "nth", |_kernel, [index, value]| {
            let index = index.require_nonnegative_usize("nth", 1)?;
            let items = value
                .as_list()
                .ok_or_else(|| "nth: argument 2 must be a list".to_string())?;
            items.get(index).cloned().ok_or_else(|| {
                NativeError::InvalidArgument(format!(
                    "nth: index {} out of bounds (len {})",
                    index,
                    items.len()
                ))
            })
        });
        exact_native!(self, "length", |_kernel, [value]| {
            let length = match value {
                Value::List(items) => items.len(),
                Value::String(value) => value.chars().count(),
                _ => return Err("length: expected list or string".into()),
            };
            i64::try_from(length)
                .map(Value::Int)
                .map_err(|_| "length: value is too large".into())
        });
        exact_native!(self, "map/get", |_kernel, [value, key]| {
            let map = value
                .as_map()
                .ok_or_else(|| "map/get: argument 1 must be a map".to_string())?;
            Ok(map.get(key).cloned().unwrap_or(Value::Nil))
        });
        exact_native!(self, "vector/get", |_kernel, [value, index]| {
            let vector = value
                .as_vector()
                .ok_or_else(|| "vector/get: argument 1 must be a vector".to_string())?;
            let index = index.require_nonnegative_usize("vector/get", 2)?;
            vector.get(index).cloned().ok_or_else(|| {
                NativeError::InvalidArgument(format!(
                    "vector/get: index {} out of bounds (len {})",
                    index,
                    vector.len()
                ))
            })
        });
        self.define_native("append", Arity::Variadic, |_kernel, args| {
            let mut result = Vec::new();
            for arg in args {
                match arg {
                    Value::List(items) => result.extend(items),
                    other => result.push(other),
                }
            }
            Ok(Value::List(result))
        });
        exact_native!(self, "kernel/error", |_kernel, [value]| Err(
            NativeError::Failed(value.to_string())
        ));
        exact_native!(self, "string-search", |_kernel, [needle, haystack]| {
            let needle = needle.require_string("string-search", 1)?;
            let haystack = haystack.require_string("string-search", 2)?;
            if let Some(byte_index) = haystack.find(needle) {
                i64::try_from(haystack[..byte_index].chars().count())
                    .map(Value::Int)
                    .map_err(|_| "string-search: index is too large".into())
            } else {
                Ok(Value::Bool(false))
            }
        });
        exact_native!(self, "substring", |_kernel, [value, start, end]| {
            let value = value.require_string("substring", 1)?;
            let start = start.require_nonnegative_usize("substring", 2)?;
            let end = end.require_nonnegative_usize("substring", 3)?;
            if start > end {
                return Err(NativeError::Failed(format!(
                    "substring: start index {} exceeds end index {}",
                    start, end
                )));
            }
            let scalar_count = value.chars().count();
            if end > scalar_count {
                return Err(NativeError::Failed(format!(
                    "substring: index {} out of bounds (len {})",
                    end, scalar_count
                )));
            }
            let offset = |index| {
                value
                    .char_indices()
                    .nth(index)
                    .map_or(value.len(), |(i, _)| i)
            };
            Ok(Value::string(&value[offset(start)..offset(end)]))
        });
    }
}
