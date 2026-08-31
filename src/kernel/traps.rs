use crate::kernel::native::exact_native;
use crate::kernel::{Kernel, VmTrap};
use crate::vm::value::{NativeError, Value};

impl Kernel {
    fn require_top_level(&self, name: &str, continuation_limited: bool) -> Result<(), NativeError> {
        if self.current_form_is(name) {
            return Ok(());
        }
        let suffix = if continuation_limited {
            " until VM continuations are explicit"
        } else {
            ""
        };
        Err(format!("{name} must be a top-level form{suffix}").into())
    }

    fn suspend(&mut self, trap: VmTrap) -> Result<Value, NativeError> {
        self.set_trap(trap).map_err(|error| error.to_string())?;
        Ok(Value::keyword("suspended"))
    }

    pub(crate) fn register_trap_builtins(&mut self) {
        exact_native!(self, "agent/return", |kernel, [value]| {
            kernel.require_top_level("agent/return", false)?;
            if kernel.frames.len() <= 1 {
                return Err("agent/return: root frame has no parent".into());
            }
            let value = value.require_string("agent/return", 1)?;
            kernel.suspend(VmTrap::ReturnAgent {
                value: Value::string(value),
            })
        });
        exact_native!(self, "message/reply", |kernel, [message_id, text]| {
            kernel.require_top_level("message/reply", false)?;
            let message_id = crate::ids::MessageId::new(message_id.coerce_text());
            let text = text.coerce_text();
            if !kernel.has_pending_message(&message_id) {
                return Err(NativeError::Failed(format!(
                    "message/reply: unknown or completed message '{}'",
                    message_id
                )));
            }
            kernel.suspend(VmTrap::Reply { message_id, text })
        });
        exact_native!(self, "model/call", |kernel, [prompt]| {
            kernel.require_top_level("model/call", false)?;
            kernel.suspend(VmTrap::CallModel {
                prompt: prompt.coerce_text(),
            })
        });
        exact_native!(self, "human/wait", |kernel, []| {
            kernel.require_top_level("human/wait", false)?;
            kernel.suspend(VmTrap::AwaitHuman)
        });
        exact_native!(self, "bash", |kernel, [command]| {
            kernel.require_top_level("bash", true)?;
            let command = match command {
                Value::String(command) => command.clone(),
                other => {
                    return Err(NativeError::Failed(format!(
                        "bash: expected command string, got {}",
                        other
                    )));
                }
            };
            kernel.suspend(VmTrap::RunBash { command })
        });
        exact_native!(self, "agent/call", |kernel, [name, request]| {
            kernel.require_top_level("agent/call", true)?;
            let name = name.require_string("agent/call", 1)?.to_owned();
            let request = request.require_string("agent/call", 2)?.to_owned();
            kernel.suspend(VmTrap::CallAgent { name, request })
        });
    }
}
