use crate::kernel::Kernel;
use crate::vm::value::{NativeError, Value};

impl Kernel {
    pub(crate) fn register_trap_builtins(&mut self) {
        self.define_native("agent/return", 1, |kernel, args| {
            if !kernel.current_form_is("agent/return") {
                return Err("agent/return must be a top-level form".into());
            }
            if kernel.frames.len() <= 1 {
                return Err("agent/return: root frame has no parent".into());
            }
            let value = args.into_iter().next().unwrap_or(Value::Nil);
            kernel
                .set_trap(crate::kernel::VmTrap::ReturnAgent { value })
                .map_err(|error| error.to_string())?;
            Ok(Value::keyword("suspended"))
        });
        self.define_native("message/reply", 2, |kernel, args| {
            if !kernel.current_form_is("message/reply") {
                return Err("message/reply must be a top-level form".into());
            }
            let message_id = crate::ids::MessageId::new(match &args[0] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            });
            let text = match &args[1] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            if !kernel.has_pending_message(&message_id) {
                return Err(NativeError::Failed(format!(
                    "message/reply: unknown or completed message '{}'",
                    message_id
                )));
            }
            kernel
                .set_trap(crate::kernel::VmTrap::Reply { message_id, text })
                .map_err(|error| error.to_string())?;
            Ok(Value::keyword("suspended"))
        });
        self.define_native("model/call", 1, |kernel, args| {
            if !kernel.current_form_is("model/call") {
                return Err("model/call must be a top-level form".into());
            }
            let prompt = match &args[0] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            kernel
                .set_trap(crate::kernel::VmTrap::CallModel { prompt })
                .map_err(|error| error.to_string())?;
            Ok(Value::keyword("suspended"))
        });
        self.define_native("human/wait", 0, |kernel, _args| {
            if !kernel.current_form_is("human/wait") {
                return Err("human/wait must be a top-level form".into());
            }
            kernel
                .set_trap(crate::kernel::VmTrap::AwaitHuman)
                .map_err(|error| error.to_string())?;
            Ok(Value::keyword("suspended"))
        });
        self.define_native("bash", 1, |kernel, args| {
            if !kernel.current_form_is("bash") {
                return Err(
                    "bash must be a top-level form until VM continuations are explicit".into(),
                );
            }
            let command = match &args[0] {
                Value::String(s) => s.clone(),
                other => {
                    return Err(NativeError::Failed(format!(
                        "bash: expected command string, got {}",
                        other
                    )));
                }
            };
            kernel
                .set_trap(crate::kernel::VmTrap::RunBash { command })
                .map_err(|error| error.to_string())?;
            Ok(Value::keyword("suspended"))
        });
        self.define_native("agent/call", 2, |kernel, args| {
            if !kernel.current_form_is("agent/call") {
                return Err(
                    "agent/call must be a top-level form until VM continuations are explicit"
                        .into(),
                );
            }
            let name = match &args[0] {
                Value::String(s) | Value::Symbol(s) => s.clone(),
                other => {
                    return Err(NativeError::Failed(format!(
                        "agent/call: expected agent name, got {}",
                        other
                    )));
                }
            };
            let request = match &args[1] {
                Value::String(s) => s.clone(),
                other => format!("{}", other),
            };
            kernel
                .set_trap(crate::kernel::VmTrap::CallAgent { name, request })
                .map_err(|error| error.to_string())?;
            Ok(Value::keyword("suspended"))
        });
    }
}
