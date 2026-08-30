
use persistent_lisp_harness::Kernel;
use std::boxed::Box;
use persistent_lisp_harness::Value;

fn make_kernel() -> &'static mut Kernel {
    let kernel = Kernel::new();
    let leaked: &'static mut Kernel = Box::leak(Box::new(kernel));
    persistent_lisp_harness::vm::eval::KERNEL.store(
        leaked as *mut Kernel,
        std::sync::atomic::Ordering::Release
    );
    leaked
}
#[test]
fn test_kernel_basics() {
    let k = make_kernel();

    let r = k.eval("(define x 42)");
    assert!(r.is_ok(), "define x: {:?}", r.err());

    let r = k.eval("x");
    assert!(r.is_ok(), "read x: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::Int(42));

    let r = k.eval("(+ 1 2)");
    assert!(r.is_ok(), "+: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::Int(3));

    let r = k.eval("(if #t 1 2)");
    assert!(r.is_ok(), "if: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::Int(1));

    let r = k.eval("(define (add a b) (+ a b))");
    assert!(r.is_ok(), "define add: {:?}", r.err());

    let r = k.eval("(add 3 4)");
    println!("add 3 4: {:?}", r);
    assert!(r.is_ok(), "call add: {:?}", r.err());

    let r = k.eval("(list 1 2 3)");
    assert!(r.is_ok(), "list: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
}

#[test]
fn test_quote_quasiquote() {
    let k = make_kernel();
    let r = k.eval("'(1 2 3)");
    assert!(r.is_ok(), "quote: {:?}", r.err());

    let r = k.eval("(let ((x 42)) `(1 ,x 3))");
    assert!(r.is_ok(), "quasiquote: {:?}", r.err());
    let v = r.unwrap();
    println!("quasiquote result: {}", v);
    assert_eq!(v, Value::list(vec![Value::Int(1), Value::Int(42), Value::Int(3)]));
}

#[test]
fn test_let_and_lexical_scope() {
    let k = make_kernel();
    let r = k.eval("(let ((x 10) (y 20)) (+ x y))");
    println!("let result: {:?}", r);
    assert!(r.is_ok(), "let: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::Int(30));
}

#[test]
fn test_snapshot_roundtrip() {
    let k = make_kernel();
    k.eval("(define answer 42)").unwrap();
    k.eval("(define greeting \"hello\")").unwrap();  // note: no quotes needed for symbol

    let json = serde_json::to_string(&k).unwrap();
    println!("Kernel JSON size: {} bytes", json.len());

    let mut recovered: Kernel = serde_json::from_str(&json).unwrap();
    let r = recovered.eval("answer").unwrap();
    assert_eq!(r, Value::Int(42));
    let r = recovered.eval("greeting").unwrap();
    assert_eq!(r, Value::string("hello"));
    println!("Snapshot roundtrip via JSON: OK");
}
