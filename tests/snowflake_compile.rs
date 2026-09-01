use persistent_lisp_harness::snowflake::compile::compile;
use persistent_lisp_harness::snowflake::runtime::CANCEL_LISP;
use persistent_lisp_harness::snowflake::value::{Capture, Op, Value};
use persistent_lisp_harness::snowflake::world::World;
use std::sync::atomic::AtomicU8;

fn compile_source(
    world: &mut World,
    source: &str,
) -> persistent_lisp_harness::snowflake::value::ChunkId {
    compile(world, source, &AtomicU8::new(0)).expect("source should compile")
}

#[test]
fn reader_requires_exactly_one_well_formed_form() {
    let cases = [
        ("", "expected one form"),
        ("1 2", "trailing input"),
        ("({)}", "mismatched"),
        ("{1}", "even number"),
        ("'", "unterminated"),
        ("\"no", "unterminated string"),
        ("`x", "unsupported reader prefix"),
    ];
    for (source, expected) in cases {
        let mut world = World::default();
        let failure = compile(&mut world, source, &AtomicU8::new(0)).unwrap_err();
        assert!(failure.message.contains(expected), "{source:?}: {failure}");
        assert!(
            world.state.code.is_empty(),
            "failed compilation inserted code"
        );
        assert!(
            world
                .state
                .symbols
                .name(persistent_lisp_harness::snowflake::value::SymbolId(0))
                .is_none()
        );
    }
}

#[test]
fn emits_collections_branches_and_tail_calls_with_checked_stack() {
    let mut world = World::default();
    let entry = compile_source(&mut world, "(if true (f 1) (begin (list 2) {:k 3}))");
    let chunk = world.chunk(entry).unwrap();
    assert!(chunk.code.iter().any(|op| matches!(op, Op::JumpFalse(_))));
    assert!(chunk.code.iter().any(|op| matches!(op, Op::TailCall(1))));
    assert!(chunk.code.iter().any(|op| matches!(op, Op::Map(1))));
    assert_eq!(chunk.code.last(), Some(&Op::Return));
    assert!(chunk.max_stack >= 2);
}

#[test]
fn propagates_captures_across_multiple_lambda_levels() {
    let mut world = World::default();
    let entry = compile_source(
        &mut world,
        "(lambda (x) (lambda () (lambda () (set! x 2))))",
    );
    let root = world.chunk(entry).unwrap();
    let Op::Closure(outer_id) = root.code[0] else {
        panic!("root must create the outer closure")
    };
    let outer = world.chunk(outer_id).unwrap();
    let Op::Closure(middle_id) = outer.code[0] else {
        panic!("outer must create the middle closure")
    };
    let middle = world.chunk(middle_id).unwrap();
    let Op::Closure(inner_id) = middle.code[0] else {
        panic!("middle must create the inner closure")
    };
    let inner = world.chunk(inner_id).unwrap();
    assert_eq!(middle.captures, vec![Capture::Local(0)]);
    assert_eq!(inner.captures, vec![Capture::Parent(0)]);
    assert!(inner.code.iter().any(|op| matches!(op, Op::SetCapture(0))));
}

#[test]
fn compiles_let_variants_and_function_definition() {
    let mut world = World::default();
    let entry = compile_source(
        &mut world,
        "(begin (define (id x) x) (let ((x 1)) (let* ((y x)) (letrec ((f (lambda () y))) (f)))))",
    );
    let chunk = world.chunk(entry).unwrap();
    assert!(chunk.code.iter().any(|op| matches!(op, Op::DefGlobal(_))));
    assert!(chunk.code.iter().any(|op| matches!(op, Op::SetLocal(_))));
    assert!(chunk.code.iter().any(|op| matches!(op, Op::TailCall(0))));
}

#[test]
fn cancellation_and_compile_errors_are_transactional() {
    let mut world = World::default();
    world.install_host(
        "fixed",
        persistent_lisp_harness::snowflake::value::HostId(7),
    );
    let before = serde_json::to_string(&world.state).unwrap();
    let failure = compile(&mut world, "(lambda (x x) x)", &AtomicU8::new(0)).unwrap_err();
    assert!(failure.message.contains("duplicate parameter"));
    assert_eq!(serde_json::to_string(&world.state).unwrap(), before);
    let failure = compile(&mut world, "1", &AtomicU8::new(CANCEL_LISP)).unwrap_err();
    assert!(failure.message.contains("cancelled"));
    assert_eq!(serde_json::to_string(&world.state).unwrap(), before);
    assert!(matches!(
        world.state.globals.values().next().unwrap().value,
        Value::Host(_)
    ));
    assert!(!world.state.globals.values().next().unwrap().mutable);
}
