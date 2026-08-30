use crate::kernel::Kernel;
use crate::kernel::native::{argument, arithmetic, index_argument, numbers, string_argument};
use crate::vm::value::{NativeError, Value};

impl Kernel {
    pub(crate) fn register_vm_primitives(&mut self) {
        self.define_native("kernel/+", 2, |_kernel, args| {
            arithmetic(&args, "+", i64::checked_add, |a, b| a + b)
        });
        self.define_native("kernel/-", 2, |_kernel, args| {
            arithmetic(&args, "-", i64::checked_sub, |a, b| a - b)
        });
        self.define_native("kernel/*", 2, |_kernel, args| {
            arithmetic(&args, "*", i64::checked_mul, |a, b| a * b)
        });
        self.define_native("kernel//", 2, |_kernel, args| {
            let (a, b) = numbers(&args, "/")?;
            if b == 0.0 {
                Err("/: division by zero".into())
            } else {
                Ok(Value::Float(a / b))
            }
        });
        self.define_native("kernel/=", 2, |_kernel, args| {
            Ok(Value::Bool(args[0] == args[1]))
        });
        self.define_native("kernel/<", 2, |_kernel, args| {
            let (a, b) = numbers(&args, "<")?;
            Ok(Value::Bool(a < b))
        });
        self.define_native("kernel/>", 2, |_kernel, args| {
            let (a, b) = numbers(&args, ">")?;
            Ok(Value::Bool(a > b))
        });
        self.define_native("kernel/cons", 2, |_kernel, args| {
            let car = args[0].clone();
            let cdr = args[1].clone();
            match cdr {
                Value::List(mut items) => {
                    let mut new_list = vec![car];
                    new_list.append(&mut items);
                    Ok(Value::List(new_list))
                }
                Value::Nil => Ok(Value::List(vec![car])),
                _ => Err("cons: second argument must be a list".into()),
            }
        });
        self.define_native("kernel/car", 1, |_kernel, args| match &args[0] {
            Value::List(items) => items
                .first()
                .cloned()
                .ok_or_else(|| "car: empty list".into()),
            _ => Err("car: expected list".into()),
        });
        self.define_native("kernel/cdr", 1, |_kernel, args| match &args[0] {
            Value::List(items) if items.len() >= 2 => Ok(Value::List(items[1..].to_vec())),
            Value::List(_) => Ok(Value::Nil),
            _ => Err("cdr: expected list".into()),
        });
        self.define_variadic_native("kernel/list", |_kernel, args| {
            Ok(Value::List(args.to_vec()))
        });
        self.define_native("kernel/display", 1, |kernel, args| {
            kernel.write_output(&format!("{}", args[0]));
            Ok(args[0].clone())
        });
        self.define_native("kernel/println", 1, |kernel, args| {
            kernel.write_output(&format!("{}\n", args[0]));
            Ok(args[0].clone())
        });
        self.define_native("kernel/nil?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(args[0], Value::Nil)))
        });
        self.define_native("kernel/number?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(
                args[0],
                Value::Int(_) | Value::Float(_)
            )))
        });
        self.define_native("kernel/symbol?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(args[0], Value::Symbol(_))))
        });
        self.define_native("kernel/string?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(args[0], Value::String(_))))
        });
        self.define_native("kernel/list?", 1, |_kernel, args| {
            Ok(Value::Bool(args[0].is_list()))
        });
        self.define_native("kernel/function?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(args[0], Value::Function(_))))
        });
        self.define_native("kernel/keyword?", 1, |_kernel, args| {
            Ok(Value::Bool(matches!(args[0], Value::Keyword(_))))
        });
        self.define_native("string-append", 2, |_kernel, args| {
            let a = match &args[0] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            let b = match &args[1] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            Ok(Value::string(&format!("{}{}", a, b)))
        });
        self.define_native("nth", 2, |_kernel, args| {
            let index = index_argument(&args, 0, "nth")?;
            let items = argument(&args, 1, "nth")?
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
        self.define_native("length", 1, |_kernel, args| {
            let length = match argument(&args, 0, "length")? {
                Value::List(items) => items.len(),
                Value::String(value) => value.chars().count(),
                _ => return Err("length: expected list or string".into()),
            };
            i64::try_from(length)
                .map(Value::Int)
                .map_err(|_| "length: value is too large".into())
        });
        self.define_native("map/get", 2, |_kernel, args| {
            let map = argument(&args, 0, "map/get")?
                .as_map()
                .ok_or_else(|| "map/get: argument 1 must be a map".to_string())?;
            let key = argument(&args, 1, "map/get")?;
            Ok(map.get(key).cloned().unwrap_or(Value::Nil))
        });
        self.define_native("vector/get", 2, |_kernel, args| {
            let vector = argument(&args, 0, "vector/get")?
                .as_vector()
                .ok_or_else(|| "vector/get: argument 1 must be a vector".to_string())?;
            let index = index_argument(&args, 1, "vector/get")?;
            vector.get(index).cloned().ok_or_else(|| {
                NativeError::InvalidArgument(format!(
                    "vector/get: index {} out of bounds (len {})",
                    index,
                    vector.len()
                ))
            })
        });
        self.define_variadic_native("append", |_kernel, args| {
            let mut result = Vec::new();
            for arg in args {
                match arg {
                    Value::List(items) => result.extend(items),
                    other => result.push(other),
                }
            }
            Ok(Value::List(result))
        });
        self.define_native("kernel/error", 1, |_kernel, args| {
            let msg = format!("{}", args[0]);
            Err(NativeError::Failed(msg))
        });
        self.define_native("string-search", 2, |_kernel, args| {
            let needle = string_argument(&args, 0, "string-search")?;
            let haystack = string_argument(&args, 1, "string-search")?;
            if let Some(byte_index) = haystack.find(needle) {
                let scalar_index = haystack[..byte_index].chars().count();
                i64::try_from(scalar_index)
                    .map(Value::Int)
                    .map_err(|_| "string-search: index is too large".into())
            } else {
                Ok(Value::Bool(false))
            }
        });
        self.define_native("substring", 3, |_kernel, args| {
            let value = string_argument(&args, 0, "substring")?;
            let start = index_argument(&args, 1, "substring")?;
            let end = index_argument(&args, 2, "substring")?;
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

            let start_byte = value
                .char_indices()
                .nth(start)
                .map_or(value.len(), |(index, _)| index);
            let end_byte = value
                .char_indices()
                .nth(end)
                .map_or(value.len(), |(index, _)| index);
            Ok(Value::string(&value[start_byte..end_byte]))
        });
    }
}
