
use persistent_lisp_harness::Kernel;

use persistent_lisp_harness::Value;

fn make_kernel() -> &'static mut Kernel {
    let k = Kernel::new();
    Box::leak(Box::new(k))
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
fn test_quasiquote() {
    let k = make_kernel();

    let r = k.eval("'(1 2 3)");
    assert!(r.is_ok(), "quote: {:?}", r.err());

    let r = k.eval("(let ((x 42)) `(1 ,x 3))");
    assert!(r.is_ok(), "quasiquote: {:?}", r.err());
    let v = r.unwrap();
    assert_eq!(v, Value::list(vec![Value::Int(1), Value::Int(42), Value::Int(3)]));
}

#[test]
fn test_lexical_scope() {
    let k = make_kernel();

    let r = k.eval("(let ((x 10) (y 20)) (+ x y))");
    assert!(r.is_ok(), "let: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::Int(30));
}

#[test]
fn test_snapshot_roundtrip() {
    let k = make_kernel();

    k.eval("(define answer 42)").unwrap();
    k.eval("(define greeting \"hello\")").unwrap();

    // Serialize both kernel and env to JSON
    let state = serde_json::json!({"kernel": &k, "env": &k.env});
    let json = serde_json::to_string(&state).unwrap();
    println!("Snapshot JSON size: {} bytes", json.len());

    // Deserialize back
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let mut recovered: Kernel = serde_json::from_value(parsed["kernel"].clone()).unwrap();
    recovered.register_tools();

    let r = recovered.eval("answer").unwrap();
    assert_eq!(r, Value::Int(42));

    let r = recovered.eval("greeting").unwrap();
    assert_eq!(r, Value::string("hello"));
    println!("Snapshot roundtrip via JSON: OK");
}
