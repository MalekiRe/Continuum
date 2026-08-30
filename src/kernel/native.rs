use crate::kernel::Kernel;
use crate::vm::value::{NativeError, Value};

pub(crate) fn argument<'a>(
    args: &'a [Value],
    index: usize,
    name: &str,
) -> Result<&'a Value, NativeError> {
    args.get(index).ok_or_else(|| {
        NativeError::InvalidArgument(format!("{}: missing argument {}", name, index + 1))
    })
}

pub(crate) fn integer_argument(
    args: &[Value],
    index: usize,
    name: &str,
) -> Result<i64, NativeError> {
    argument(args, index, name)?.require_int(name, index + 1)
}

pub(crate) fn index_argument(
    args: &[Value],
    index: usize,
    name: &str,
) -> Result<usize, NativeError> {
    argument(args, index, name)?.require_nonnegative_usize(name, index + 1)
}

pub(crate) fn string_argument<'a>(
    args: &'a [Value],
    index: usize,
    name: &str,
) -> Result<&'a str, NativeError> {
    argument(args, index, name)?.require_string(name, index + 1)
}

pub(crate) fn numbers(args: &[Value], name: &str) -> Result<(f64, f64), NativeError> {
    Ok((
        argument(args, 0, name)?.require_number(name, 1)?,
        argument(args, 1, name)?.require_number(name, 2)?,
    ))
}

pub(crate) fn arithmetic(
    args: &[Value],
    name: &str,
    ints: fn(i64, i64) -> Option<i64>,
    floats: fn(f64, f64) -> f64,
) -> Result<Value, NativeError> {
    if let (Some(Value::Int(a)), Some(Value::Int(b))) = (args.first(), args.get(1)) {
        return ints(*a, *b)
            .map(Value::Int)
            .ok_or_else(|| NativeError::InvalidArgument(format!("{}: integer overflow", name)));
    }
    let (a, b) = numbers(args, name)?;
    Ok(Value::Float(floats(a, b)))
}

impl Kernel {
    pub fn register_tools(&mut self) {
        // Snapshots may contain natives retired after they were written.
        if let Some(namespace) =
            std::sync::Arc::make_mut(&mut self.env.namespaces).get_mut("kernel")
        {
            for name in ["read", "sleep"] {
                namespace.bindings.shift_remove(name);
                namespace.sources.shift_remove(name);
            }
        }
        self.register_vm_primitives();
        self.register_kernel_builtins();
        self.register_trap_builtins();
    }
}
