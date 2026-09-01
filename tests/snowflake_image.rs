use persistent_lisp_harness::snowflake::effects;
use persistent_lisp_harness::snowflake::image::{ImageError, ImageStore};
use persistent_lisp_harness::snowflake::runtime::PAUSE;
use persistent_lisp_harness::snowflake::value::{Capture, Chunk, ChunkId, Op, Value};
use persistent_lisp_harness::snowflake::vm::{Task, TaskPoll};
use persistent_lisp_harness::snowflake::world::{Agent, Binding, World};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

fn world() -> World {
    let mut world = World::default();
    effects::install(&mut world);
    world
        .state
        .agents
        .push(Agent::new("Continuum".into(), String::new()));
    world
}

fn directory() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("snowflake-image-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn newest_corrupted_slot_falls_back_to_previous_atomic_image() {
    let directory = directory();
    let store = ImageStore::new(&directory);
    let mut world = world();
    let symbol = world.state.symbols.intern("answer");
    world.state.globals.insert(
        symbol,
        Binding {
            value: Value::Int(1),
            source: None,
            mutable: true,
        },
    );
    store.save(&world, None).unwrap();
    world.state.globals.get_mut(&symbol).unwrap().value = Value::Int(2);
    store.save(&world, None).unwrap();
    std::fs::write(directory.join("slot-1.json"), b"corrupt").unwrap();
    let recovered = store.load().unwrap();
    assert_eq!(recovered.state.globals[&symbol].value, Value::Int(1));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn snapshot_of_paused_task_uses_committed_view_not_live_lisp_mutations() {
    let directory = directory();
    let store = ImageStore::new(&directory);
    let mut world = world();
    let control = Arc::new(AtomicU8::new(0));
    let entry = persistent_lisp_harness::snowflake::compile::compile(
        &mut world,
        "(begin (define uncommitted 9) (letrec ((loop (lambda () (loop)))) (loop)))",
        &control,
    )
    .unwrap();
    let mut task = Task::start(&world, entry).unwrap();
    let interrupt = control.clone();
    let pauser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        interrupt.store(PAUSE, Ordering::Release);
    });
    assert!(matches!(task.poll(&mut world, &control), TaskPoll::Paused));
    pauser.join().unwrap();
    let symbol = world.state.symbols.intern("uncommitted");
    assert!(world.state.globals.contains_key(&symbol));
    store.save(&world, Some(task.transaction())).unwrap();
    let recovered = store.load().unwrap();
    assert!(!recovered.state.globals.contains_key(&symbol));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn save_rejects_dangling_current_format_references() {
    let directory = directory();
    let store = ImageStore::new(&directory);
    let mut world = world();
    let symbol = world.state.symbols.intern("bad");
    world.state.globals.insert(
        symbol,
        Binding {
            value: Value::Closure {
                chunk: ChunkId(99),
                captures: Vec::new(),
            },
            source: None,
            mutable: true,
        },
    );
    assert!(matches!(
        store.save(&world, None),
        Err(ImageError::Invalid(_))
    ));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn images_reject_malformed_reserved_hosts_and_unknown_nested_fields() {
    let directory = directory();
    let store = ImageStore::new(&directory);
    let mut malformed = world();
    let plus = malformed.state.symbols.get("+").unwrap();
    malformed.state.globals.get_mut(&plus).unwrap().value = Value::Int(7);
    assert!(matches!(
        store.save(&malformed, None),
        Err(ImageError::Invalid(_))
    ));

    let world = world();
    store.save(&world, None).unwrap();
    let path = directory.join("slot-0.json");
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json["state"]["agents"][0]["unknown"] = serde_json::json!(true);
    std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert!(matches!(store.load(), Err(ImageError::Invalid(_))));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn images_reject_unboxed_and_mismatched_closure_captures() {
    let directory = directory();
    let store = ImageStore::new(&directory);
    let mut world = world();
    let name = world.state.symbols.intern("bad-closure");
    let child = Chunk {
        name,
        source: String::new(),
        arity: 0,
        locals: 0,
        max_stack: 1,
        boxed: Vec::new(),
        captures: vec![Capture::Local(0)],
        constants: Vec::new(),
        code: vec![Op::GetCapture(0), Op::Return],
    };
    world.state.code.push(Some(child));
    world.state.code.push(Some(Chunk {
        name,
        source: String::new(),
        arity: 0,
        locals: 1,
        max_stack: 1,
        boxed: Vec::new(),
        captures: Vec::new(),
        constants: Vec::new(),
        code: vec![Op::Closure(ChunkId(0)), Op::Return],
    }));
    assert!(matches!(
        store.save(&world, None),
        Err(ImageError::Invalid(_))
    ));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn images_validate_unreachable_bytecode_tails() {
    let directory = directory();
    let store = ImageStore::new(&directory);
    let mut world = world();
    let name = world.state.symbols.intern("unreachable-bad-code");
    world.state.code.push(Some(Chunk {
        name,
        source: String::new(),
        arity: 0,
        locals: 0,
        max_stack: 1,
        boxed: Vec::new(),
        captures: Vec::new(),
        constants: vec![Value::Nil],
        code: vec![Op::Const(0), Op::Return, Op::GetCapture(0)],
    }));
    assert!(matches!(
        store.save(&world, None),
        Err(ImageError::Invalid(_))
    ));
    std::fs::remove_dir_all(directory).unwrap();
}
