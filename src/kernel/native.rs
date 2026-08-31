macro_rules! exact_native {
    ($kernel:expr, $name:literal, |$context:ident, [$($argument:ident),* $(,)?]| $body:expr) => {{
        const ARITY: u32 = 0 $(+ { let _ = stringify!($argument); 1 })*;
        $kernel.define_native(
            $name,
            $crate::vm::value::Arity::Exact(ARITY),
            |$context, arguments| match arguments.as_slice() {
                [$($argument),*] => $body,
                _ => Err($crate::vm::value::NativeError::InvalidArgument(format!(
                    "{}: expected {} arguments", $name, ARITY
                ))),
            },
        );
    }};
}
pub(crate) use exact_native;

use crate::kernel::Kernel;
use crate::vm::value::{NativeError, Value};

pub(crate) fn numbers(left: &Value, right: &Value, name: &str) -> Result<(f64, f64), NativeError> {
    Ok((
        left.require_number(name, 1)?,
        right.require_number(name, 2)?,
    ))
}

pub(crate) fn arithmetic(
    left: &Value,
    right: &Value,
    name: &str,
    ints: fn(i64, i64) -> Option<i64>,
    floats: fn(f64, f64) -> f64,
) -> Result<Value, NativeError> {
    if let (Value::Int(a), Value::Int(b)) = (left, right) {
        return ints(*a, *b)
            .map(Value::Int)
            .ok_or_else(|| NativeError::InvalidArgument(format!("{}: integer overflow", name)));
    }
    let (a, b) = numbers(left, right, name)?;
    Ok(Value::Float(floats(a, b)))
}

impl Kernel {
    pub(crate) fn register_tools(&mut self) {
        self.natives.clear();
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
