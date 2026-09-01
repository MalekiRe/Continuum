use crate::executor::{ExecutionOutcome, Executor, ExecutorConfig};
use crate::scheduler::{ModelClient, ModelRequest, OpenRouterModel};
use crate::snowflake::value::{HostId, MessageId, Value};
use crate::snowflake::world::World;
use std::future::Future;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq)]
pub enum EffectRequest {
    Bash(String),
    Model(String),
    Agent { name: String, request: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum TerminalEffect {
    Reply { message: MessageId, text: String },
    ReturnAgent(String),
}

#[derive(Debug, PartialEq)]
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
    pub fn new(future: ExternalFuture, cancel: impl Fn() + Send + Sync + 'static) -> Self {
        let cancel = Box::new(cancel);
        Self { future, cancel }
    }

    pub fn cancel(&self) {
        (self.cancel)();
    }
}

const ADD: HostId = HostId(0);
const SUBTRACT: HostId = HostId(1);
const MULTIPLY: HostId = HostId(2);
const DIVIDE: HostId = HostId(3);
const EQUAL: HostId = HostId(4);
const LESS: HostId = HostId(5);
const LIST: HostId = HostId(7);
const CONS: HostId = HostId(8);
const FIRST: HostId = HostId(9);
const REST: HostId = HostId(10);
const LENGTH: HostId = HostId(11);
const STRING_APPEND: HostId = HostId(12);
const BASH: HostId = HostId(13);
const MODEL: HostId = HostId(14);
const AGENT: HostId = HostId(15);
const REPLY: HostId = HostId(16);
pub const RETURN: HostId = HostId(17);

const HOSTS: &[(&str, HostId)] = &[
    ("+", ADD),
    ("-", SUBTRACT),
    ("*", MULTIPLY),
    ("/", DIVIDE),
    ("=", EQUAL),
    ("<", LESS),
    ("list", LIST),
    ("cons", CONS),
    ("first", FIRST),
    ("rest", REST),
    ("length", LENGTH),
    ("string-append", STRING_APPEND),
    ("bash", BASH),
    ("model", MODEL),
    ("agent", AGENT),
    ("reply", REPLY),
    ("return", RETURN),
];

pub fn install(world: &mut World) {
    for &(name, host) in HOSTS {
        world.install_host(name, host);
    }
}

pub(crate) fn valid_host(host: HostId) -> bool {
    HOSTS.iter().any(|(_, registered)| *registered == host)
}

pub(crate) fn valid_prelude(world: &World) -> bool {
    HOSTS.iter().all(|(name, host)| {
        world
            .state
            .symbols
            .get(name)
            .and_then(|id| world.state.globals.get(&id))
            .is_some_and(|binding| binding.value == Value::Host(*host) && !binding.mutable)
    })
}

pub fn call(
    world: &mut World,
    host: HostId,
    arguments: Vec<Value>,
) -> Result<HostResult, EffectError> {
    let value = match host {
        ADD => arithmetic_fold("+", &arguments, 0, i64::checked_add)?,
        MULTIPLY => arithmetic_fold("*", &arguments, 1, i64::checked_mul)?,
        SUBTRACT => numeric_subtract(&arguments)?,
        DIVIDE => numeric_divide(&arguments)?,
        EQUAL => {
            exact("=", &arguments, 2)?;
            Value::Bool(arguments[0] == arguments[1])
        }
        LESS => compare("<", &arguments)?,
        LIST => Value::List(arguments),
        CONS => {
            exact("cons", &arguments, 2)?;
            let mut tail = expect_list("cons", &arguments[1])?.to_vec();
            tail.insert(0, arguments[0].clone());
            Value::List(tail)
        }
        FIRST => {
            exact("first", &arguments, 1)?;
            expect_list("first", &arguments[0])?
                .first()
                .cloned()
                .unwrap_or(Value::Nil)
        }
        REST => {
            exact("rest", &arguments, 1)?;
            let list = expect_list("rest", &arguments[0])?;
            Value::List(list.get(1..).unwrap_or_default().to_vec())
        }
        LENGTH => {
            exact("length", &arguments, 1)?;
            let length = match &arguments[0] {
                Value::List(values) => values.len(),
                Value::String(value) => value.chars().count(),
                _ => {
                    return Err(EffectError(
                        "length expects a list, vector, or string".into(),
                    ));
                }
            };
            Value::Int(i64::try_from(length).map_err(|_| EffectError("length overflow".into()))?)
        }
        STRING_APPEND => {
            let mut result = String::new();
            for value in &arguments {
                result.push_str(expect_string("string-append", value)?);
            }
            Value::String(result)
        }
        BASH => {
            exact("bash", &arguments, 1)?;
            return Ok(HostResult::Effect(EffectRequest::Bash(
                expect_string("bash", &arguments[0])?.to_owned(),
            )));
        }
        MODEL => {
            exact("model", &arguments, 1)?;
            return Ok(HostResult::Effect(EffectRequest::Model(
                expect_string("model", &arguments[0])?.to_owned(),
            )));
        }
        AGENT => {
            exact("agent", &arguments, 2)?;
            return Ok(HostResult::Effect(EffectRequest::Agent {
                name: expect_string("agent", &arguments[0])?.to_owned(),
                request: expect_string("agent", &arguments[1])?.to_owned(),
            }));
        }
        REPLY => {
            exact("reply", &arguments, 2)?;
            let Value::Int(raw) = arguments[0] else {
                return Err(EffectError("reply expects an integer message id".into()));
            };
            let id = MessageId(
                u32::try_from(raw)
                    .map_err(|_| EffectError("reply message id is out of range".into()))?,
            );
            if world
                .state
                .agents
                .last()
                .is_none_or(|agent| !agent.inbox.contains(&id))
            {
                return Err(EffectError(
                    "reply message is not owned by this agent".into(),
                ));
            }
            let message = world
                .state
                .messages
                .get(&id)
                .ok_or_else(|| EffectError("reply message does not exist".into()))?;
            if message.answered {
                return Err(EffectError("reply message was already answered".into()));
            }
            return Ok(HostResult::Terminal(TerminalEffect::Reply {
                message: id,
                text: expect_string("reply", &arguments[1])?.to_owned(),
            }));
        }
        RETURN => {
            exact("return", &arguments, 1)?;
            return Ok(HostResult::Terminal(TerminalEffect::ReturnAgent(
                expect_string("return", &arguments[0])?.to_owned(),
            )));
        }
        _ => return Err(EffectError(format!("unknown host {}", host.0))),
    };
    Ok(HostResult::Value(value))
}

fn exact(name: &str, arguments: &[Value], expected: usize) -> Result<(), EffectError> {
    if arguments.len() == expected {
        Ok(())
    } else {
        Err(EffectError(format!(
            "{name} expects {expected} arguments, got {}",
            arguments.len()
        )))
    }
}

fn integer(name: &str, value: &Value) -> Result<i64, EffectError> {
    if let Value::Int(value) = value {
        Ok(*value)
    } else {
        Err(EffectError(format!("{name} expects integers")))
    }
}

fn arithmetic_fold(
    name: &str,
    arguments: &[Value],
    initial: i64,
    operation: impl Fn(i64, i64) -> Option<i64>,
) -> Result<Value, EffectError> {
    arguments
        .iter()
        .try_fold(initial, |result, argument| {
            operation(result, integer(name, argument)?)
                .ok_or_else(|| EffectError(format!("{name} integer overflow")))
        })
        .map(Value::Int)
}

fn numeric_subtract(arguments: &[Value]) -> Result<Value, EffectError> {
    let mut values = arguments.iter();
    let first = integer(
        "-",
        values
            .next()
            .ok_or_else(|| EffectError("- expects arguments".into()))?,
    )?;
    let overflow = || EffectError("- integer overflow".into());
    let result = if arguments.len() == 1 {
        first.checked_neg().ok_or_else(overflow)?
    } else {
        values.try_fold(first, |left, right| {
            left.checked_sub(integer("-", right)?).ok_or_else(overflow)
        })?
    };
    Ok(Value::Int(result))
}

fn numeric_divide(arguments: &[Value]) -> Result<Value, EffectError> {
    exact("/", arguments, 2)?;
    integer("/", &arguments[0])?
        .checked_div(integer("/", &arguments[1])?)
        .map(Value::Int)
        .ok_or_else(|| EffectError("invalid integer division".into()))
}

fn compare(name: &str, arguments: &[Value]) -> Result<Value, EffectError> {
    exact(name, arguments, 2)?;
    Ok(Value::Bool(
        integer(name, &arguments[0])? < integer(name, &arguments[1])?,
    ))
}

fn expect_list<'a>(name: &str, value: &'a Value) -> Result<&'a [Value], EffectError> {
    match value {
        Value::List(values) => Ok(values),
        _ => Err(EffectError(format!("{name} expects a list"))),
    }
}

fn expect_string<'a>(name: &str, value: &'a Value) -> Result<&'a str, EffectError> {
    match value {
        Value::String(value) => Ok(value),
        _ => Err(EffectError(format!("{name} expects a string"))),
    }
}

pub fn display(value: &Value) -> String {
    match value {
        Value::Nil => "nil".into(),
        Value::Bool(value) => value.to_string(),
        Value::Int(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::List(values) => format!(
            "({})",
            values.iter().map(display).collect::<Vec<_>>().join(" ")
        ),
        _ => format!("{value:?}"),
    }
}

pub fn start(effect: &EffectRequest) -> ExternalRun {
    let cancellation = CancellationToken::new();
    let cancel_token = cancellation.clone();
    match effect.clone() {
        EffectRequest::Bash(command) => {
            let executor = std::env::current_dir()
                .map_err(|error| error.to_string())
                .and_then(|directory| {
                    Executor::new(ExecutorConfig::with_working_directory(directory))
                        .map_err(|error| error.to_string())
                });
            let cancel_executor = executor.as_ref().ok().cloned();
            let future = Box::pin(async move {
                let executor = executor.map_err(EffectError)?;
                if cancellation.is_cancelled() {
                    return Err(EffectError("external effect cancelled".into()));
                }
                let running = executor.clone();
                let worker_token = cancellation.clone();
                let mut work = tokio::task::spawn_blocking(move || {
                    if worker_token.is_cancelled() {
                        Ok(None)
                    } else {
                        running.run(&command).map(Some)
                    }
                });
                let joined = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => {
                        loop {
                            if executor.is_running() {
                                let _ = executor.cancel();
                                let _ = work.await;
                                break;
                            }
                            tokio::select! {
                                result = &mut work => {
                                    let _ = result;
                                    break;
                                }
                                () = tokio::task::yield_now() => {}
                            }
                        }
                        return Err(EffectError("external effect cancelled".into()));
                    }
                    joined = &mut work => joined,
                };
                let result = joined
                    .map_err(|error| EffectError(format!("bash worker failed: {error}")))?
                    .map_err(|error| EffectError(error.to_string()))?
                    .ok_or_else(|| EffectError("external effect cancelled".into()))?;
                match result.outcome {
                    ExecutionOutcome::Cancelled => {
                        Err(EffectError("external effect cancelled".into()))
                    }
                    ExecutionOutcome::TimedOut => Err(EffectError("bash timed out".into())),
                    ExecutionOutcome::Exited if result.exit_code == 0 => {
                        Ok(Value::String(result.output.stdout))
                    }
                    ExecutionOutcome::Exited => Err(EffectError(format!(
                        "bash exited {}: {}",
                        result.exit_code, result.output.stderr
                    ))),
                }
            });
            ExternalRun {
                future,
                cancel: Box::new(move || {
                    cancel_token.cancel();
                    if let Some(executor) = &cancel_executor {
                        let _ = executor.cancel();
                    }
                }),
            }
        }
        EffectRequest::Model(context) => {
            let future_token = cancellation.clone();
            let future = Box::pin(async move {
                OpenRouterModel::default()
                    .complete(
                        ModelRequest {
                            system: String::new(),
                            context,
                        },
                        future_token,
                    )
                    .await
                    .map(Value::String)
                    .map_err(|error| EffectError(error.to_string()))
            });
            ExternalRun {
                future,
                cancel: Box::new(move || cancel_token.cancel()),
            }
        }
        EffectRequest::Agent { .. } => unreachable!("agent effects are started by Runtime"),
    }
}
