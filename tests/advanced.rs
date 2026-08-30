
use persistent_lisp_harness::Kernel;
use std::boxed::Box;
use persistent_lisp_harness::Value;

fn make_kernel() -> &'static mut Kernel {
    let k = Kernel::new();
    Box::leak(Box::new(k))
}
#[test]
fn test_define_data_and_match() {
    let k = make_kernel();

    let r = k.eval(r#"(define-data result/Result (Ok value) (Err problem))"#);
    assert!(r.is_ok(), "define-data: {:?}", r.err());

    let r = k.eval(r#"(user/result/Result/Ok 42)"#);
    assert!(r.is_ok(), "constructor: {:?}", r.err());

    let r = k.eval(r#"
        (match (user/result/Result/Ok 42)
          ((result/Result/Ok n) (+ n 1))
          ((result/Result/Err msg) -1))
    "#);

    assert!(r.is_ok(), "match: {:?}", r.err());
    let v = r.unwrap();
    assert_eq!(v, Value::Int(43));

    let r = k.eval(r#"
        (match (user/result/Result/Err "oops")
          ((result/Result/Ok n) n)
          ((result/Result/Err msg) (println msg)))
    "#);

    assert!(r.is_ok(), "match err: {:?}", r.err());
}

#[test]
fn test_macro_syntax_rules() {
    let k = make_kernel();

    let r = k.eval(r#"
        (define-syntax my-when
          (syntax-rules ()
            ((my-when test body ...)
             (if test (begin body ...) nil))))
    "#);

    assert!(r.is_ok(), "define-syntax: {:?}", r.err());

    let r = k.eval("(my-when #t 42)");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(42));

    let r = k.eval("(my-when #f 99)");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Nil);

    let r = k.eval("(my-when #t (println 1) (println 2) 42)");
    assert!(r.is_ok(), "my-when multi: {:?}", r.err());
}

#[test]
fn test_undefine_and_history() {
    let k = make_kernel();

    k.eval("(define x 10)").unwrap();
    k.eval("(define x 20)").unwrap();
    k.eval("(define x 30)").unwrap();
    assert_eq!(k.eval("x").unwrap(), Value::Int(30));

    k.eval("(undefine x)").unwrap();
    let r = k.eval("x");
    assert!(r.is_err());
}

#[test]
fn test_quasiquote_unquote() {
    let k = make_kernel();

    let r = k.eval("(let ((x 42)) `(1 ,x 3))");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::list(vec![Value::Int(1), Value::Int(42), Value::Int(3)]));

    let r = k.eval("(let ((lst '(a b c))) `(x ,@lst y))");
    assert!(r.is_ok(), "unquote-splicing: {:?}", r.err());
}
