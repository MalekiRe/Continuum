use crate::snowflake::value::{HostId, MessageId, Value};
use crate::snowflake::world::World;
use std::future::Future;
use std::pin::Pin;

#[derive(Debug)]
pub enum EffectRequest {
    Bash(String),
    Model(String),
    Agent { name: String, request: String },
}

#[derive(Debug)]
pub enum TerminalEffect {
    Reply { message: MessageId, text: String },
    ReturnAgent(String),
}

pub enum HostResult {
    Value(Value),
    Effect(EffectRequest),
    Terminal(TerminalEffect),
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct EffectError(pub String);

pub type ExternalFuture = Pin<Box<dyn Future<Output = Result<Value, EffectError>> + Send>>;

pub struct ExternalRun {
    pub future: ExternalFuture,
    cancel: Box<dyn Fn() + Send + Sync>,
}

impl ExternalRun {
    pub fn cancel(&self) {
        (self.cancel)();
    }
}

pub fn install(_world: &mut World) {
    todo!("install the minimal immutable host prelude")
}

pub fn call(
    _world: &mut World,
    _host: HostId,
    _arguments: Vec<Value>,
) -> Result<HostResult, EffectError> {
    todo!("dispatch a synchronous host or yield a typed external effect")
}

pub fn start(_effect: &EffectRequest) -> ExternalRun {
    todo!("start model/bash work with a separate explicit cancellation handle")
}
