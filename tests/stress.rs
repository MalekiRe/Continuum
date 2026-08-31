use persistent_lisp_harness::{Kernel, Value};

#[test]
fn deep_tail_recursion_uses_constant_rust_stack() {
    let mut kernel = Kernel::new();
    kernel
        .eval_value(r#"(define (count n) (if (= n 0) "done" (count (- n 1))))"#)
        .unwrap();
    assert_eq!(
        kernel.eval_value("(count 50000)").unwrap(),
        Value::string("done")
    );
}

#[test]
fn mutual_tail_recursion_uses_constant_rust_stack() {
    let mut kernel = Kernel::new();
    kernel
        .eval_value(
            r#"
        (define (even? n) (if (= n 0) #t (odd? (- n 1))))
        (define (odd? n) (if (= n 0) #f (even? (- n 1))))
    "#,
        )
        .unwrap();
    assert_eq!(
        kernel.eval_value("(even? 20000)").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn closures_remain_correct_in_a_large_environment() {
    let mut kernel = Kernel::new();
    for i in 0..100 {
        kernel
            .eval_value(&format!("(define (helper{} x) (+ x {}))", i, i))
            .unwrap();
    }
    kernel
        .eval_value("(define (make-adder n) (lambda (x) (+ x n)))")
        .unwrap();
    kernel.eval_value("(define add9 (make-adder 9))").unwrap();
    assert_eq!(kernel.eval_value("(add9 33)").unwrap(), Value::Int(42));
}

#[test]
fn reader_and_evaluator_handle_deep_expressions() {
    let mut kernel = Kernel::new();
    let expression = (2..200).fold("1".to_string(), |inner, value| {
        format!("(+ {} {})", value, inner)
    });
    assert_eq!(kernel.eval_value(&expression).unwrap(), Value::Int(19_900));
}

#[test]
fn repeated_failures_do_not_corrupt_committed_state() {
    let mut kernel = Kernel::new();
    kernel.eval_value("(define user/stable 42)").unwrap();
    for i in 0..100 {
        assert!(
            kernel
                .eval_value(&format!("(begin (define user/transient {}) (missing))", i))
                .is_err()
        );
        assert_eq!(kernel.eval_value("stable").unwrap(), Value::Int(42));
        assert!(kernel.eval_value("transient").is_err());
    }
}

#[test]
fn large_tagged_data_family_remains_usable() {
    let mut kernel = Kernel::new();
    let mut definition = String::from("(define-data many/Status");
    for i in 0..50 {
        definition.push_str(&format!(" (Variant{} x y)", i));
    }
    definition.push(')');
    kernel.eval_value(&definition).unwrap();
    assert!(matches!(
        kernel.eval_value("(many/Status/Variant49 1 2)").unwrap(),
        Value::Tagged { .. }
    ));
}
