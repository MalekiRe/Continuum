use persistent_lisp_harness::Kernel;
use persistent_lisp_harness::Value;

// ===== BASIC REPL TESTS =====

#[test]
fn test_repl_define_and_lookup() {
    let mut k = Kernel::new();
    let r = k.eval("(define x 42)");
    assert!(r.is_ok(), "define x: {:?}", r.err());
    let r = k.eval("x");
    assert!(r.is_ok(), "lookup x: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::Int(42));
}

#[test]
fn test_repl_undefine_and_errors() {
    let mut k = Kernel::new();
    let r = k.eval("(define x 42)");
    assert!(r.is_ok());
    let r = k.eval("(undefine x)");
    assert!(r.is_ok());
    let r = k.eval("x");
    assert!(r.is_err(), "should error on undefined symbol");
}

#[test]
fn test_repl_function_define_and_call() {
    let mut k = Kernel::new();
    let r = k.eval("(define (add a b) (+ a b))");
    assert!(r.is_ok());
    let r = k.eval("(add 3 4)");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(7));
}

#[test]
fn test_repl_lambda() {
    let mut k = Kernel::new();
    let r = k.eval("(define double (lambda (x) (* x 2)))");
    assert!(r.is_ok());
    let r = k.eval("(double 5)");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(10));
}

#[test]
fn test_repl_conditional() {
    let mut k = Kernel::new();
    assert_eq!(k.eval("(if #t 1 2)").unwrap(), Value::Int(1));
    assert_eq!(k.eval("(if #f 1 2)").unwrap(), Value::Int(2));
    assert_eq!(k.eval("(if nil 1 2)").unwrap(), Value::Int(2));
    assert_eq!(k.eval("(if 42 1 2)").unwrap(), Value::Int(1));
}

#[test]
fn test_repl_let_and_scope() {
    let mut k = Kernel::new();
    let r = k.eval("(let ((x 10) (y 20)) (+ x y))");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(30));
    let r = k.eval("(let* ((x 1) (y (+ x 1))) y)");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(2));
}

#[test]
fn test_repl_begin() {
    let mut k = Kernel::new();
    let r = k.eval("(begin (define a 1) (define b 2) (+ a b))");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(3));
}

#[test]
fn test_repl_set_and_mutation() {
    let mut k = Kernel::new();
    k.eval("(define x 10)").unwrap();
    k.eval("(set! x 20)").unwrap();
    assert_eq!(k.eval("x").unwrap(), Value::Int(20));
}

#[test]
fn test_repl_quote() {
    let mut k = Kernel::new();
    let r = k.eval("'(1 2 3)");
    assert!(r.is_ok());
    assert_eq!(
        r.unwrap(),
        Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
}

#[test]
fn test_repl_quasiquote() {
    let mut k = Kernel::new();
    let r = k.eval("(let ((x 42)) `(1 ,x 3))");
    assert!(r.is_ok());
    assert_eq!(
        r.unwrap(),
        Value::list(vec![Value::Int(1), Value::Int(42), Value::Int(3)])
    );
}

#[test]
fn test_repl_quasiquote_splicing() {
    let mut k = Kernel::new();
    let r = k.eval("(let ((lst '(a b c))) `(x ,@lst y))");
    assert!(r.is_ok());
    assert!(r.unwrap().is_list());
}

#[test]
fn test_repl_list_ops() {
    let mut k = Kernel::new();
    let three = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    assert_eq!(k.eval("(list 1 2 3)").unwrap(), three);
    assert_eq!(k.eval("(car (list 1 2 3))").unwrap(), Value::Int(1));
    assert_eq!(
        k.eval("(cdr (list 1 2 3))").unwrap(),
        Value::list(vec![Value::Int(2), Value::Int(3)])
    );
    assert_eq!(k.eval("(cons 1 (list 2 3))").unwrap(), three);
}

#[test]
fn test_repl_arithmetic() {
    let mut k = Kernel::new();
    assert_eq!(k.eval("(+ 1 2)").unwrap(), Value::Int(3));
    assert_eq!(k.eval("(- 5 3)").unwrap(), Value::Int(2));
    assert_eq!(k.eval("(* 2 3)").unwrap(), Value::Int(6));
    assert_eq!(k.eval("(= 5 5)").unwrap(), Value::Bool(true));
    assert_eq!(k.eval("(= 5 6)").unwrap(), Value::Bool(false));
    assert!(k.eval("(< 1 2)").unwrap().is_truthy());
    assert!(!k.eval("(> 1 2)").unwrap().is_truthy());
}

#[test]
fn test_repl_string_append() {
    let mut k = Kernel::new();
    let r = k.eval(r#"(string-append "hello " "world")"#);
    assert!(r.is_ok(), "string-append: {:?}", r.err());
    let v = r.unwrap();
    assert_eq!(v, Value::string("hello world"), "got {}", v);
}

#[test]
fn test_repl_string_search() {
    let mut k = Kernel::new();
    let r = k.eval(r#"(string-search "lisp" "hello lisp world")"#);
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(6));
    let r = k.eval(r#"(string-search "xyz" "hello world")"#);
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Bool(false));
}

#[test]
fn test_repl_substring() {
    let mut k = Kernel::new();
    let r = k.eval(r#"(substring "hello world" 0 5)"#);
    assert!(r.is_ok(), "substring: {:?}", r.err());
    let v = r.unwrap();
    assert_eq!(v, Value::string("hello"), "got {}", v);
}

#[test]
fn test_repl_bash_echo() {
    let mut k = Kernel::new();
    let r = k.eval(r#"(bash "echo hello world")"#);
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::keyword("suspended"));
    assert!(
        matches!(k.take_trap(), Some(pending) if matches!(pending.operation, persistent_lisp_harness::VmTrap::RunBash { ref command } if command == "echo hello world"))
    );
}

#[test]
fn test_repl_bash_exit_code() {
    let mut k = Kernel::new();
    let r = k.eval(r#"(bash "exit 42")"#);
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::keyword("suspended"));
    assert!(
        matches!(k.take_trap(), Some(pending) if matches!(pending.operation, persistent_lisp_harness::VmTrap::RunBash { ref command } if command == "exit 42"))
    );
}

#[test]
fn test_repl_type_predicates() {
    let mut k = Kernel::new();
    assert_eq!(k.eval("(nil? nil)").unwrap(), Value::Bool(true));
    assert_eq!(k.eval("(nil? 42)").unwrap(), Value::Bool(false));
    assert_eq!(k.eval("(number? 42)").unwrap(), Value::Bool(true));
    assert_eq!(k.eval("(symbol? 'x)").unwrap(), Value::Bool(true));
    assert_eq!(k.eval("(string? \"x\")").unwrap(), Value::Bool(true));
    assert_eq!(k.eval("(list? (list 1 2))").unwrap(), Value::Bool(true));
    assert_eq!(
        k.eval("(function? (lambda (x) x))").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn test_repl_define_data_and_match() {
    let mut k = Kernel::new();
    let r = k.eval("(define-data result/Result (Ok value) (Err problem))");
    assert!(r.is_ok());
    let r = k.eval(
        r#"(match (user/result/Result/Ok 42)
          ((result/Result/Ok n) (+ n 1))
          ((result/Result/Err msg) -1))"#,
    );
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(43));
}

#[test]
fn test_repl_macro_syntax_rules() {
    let mut k = Kernel::new();
    let r = k.eval(
        r#"
        (define-syntax my-when
          (syntax-rules ()
            ((my-when test body ...)
             (if test (begin body ...) nil))))
    "#,
    );
    assert!(r.is_ok());
    assert_eq!(k.eval("(my-when #t 42)").unwrap(), Value::Int(42));
    assert_eq!(k.eval("(my-when #f 99)").unwrap(), Value::Nil);
}

#[test]
fn test_repl_nth_and_length() {
    let mut k = Kernel::new();
    assert_eq!(k.eval("(nth 0 (list 10 20 30))").unwrap(), Value::Int(10));
    assert_eq!(k.eval("(length (list 1 2 3))").unwrap(), Value::Int(3));
    assert_eq!(k.eval(r#"(length "hello")"#).unwrap(), Value::Int(5));
}

#[test]
fn test_repl_append() {
    let mut k = Kernel::new();
    let r = k.eval("(append (list 1 2) (list 3 4))");
    assert!(r.is_ok());
    let expected = Value::list(vec![
        Value::Int(1),
        Value::Int(2),
        Value::Int(3),
        Value::Int(4),
    ]);
    assert_eq!(r.unwrap(), expected);
}

#[test]
fn test_repl_tail_recursion() {
    let mut k = Kernel::new();
    k.eval("(define (count n) (if (= n 0) \"done\" (count (- n 1))))")
        .unwrap();
    let r = k.eval("(count 10000)");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::string("done"));
}

#[test]
fn test_repl_closure() {
    let mut k = Kernel::new();
    let r = k.eval("(define (make-adder n) (lambda (x) (+ x n)))");
    assert!(r.is_ok(), "define make-adder: {:?}", r.err());
    let r = k.eval("(define add5 (make-adder 5))");
    assert!(r.is_ok(), "define add5: {:?}", r.err());
    let r = k.eval("(add5 10)");
    assert!(r.is_ok(), "add5 call: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::Int(15), "closure result");
}

#[test]
fn test_repl_agent_core_loads() {
    let mut k = Kernel::new();
    let core = r#"
        (define-data result/Result
          (Ok value)
          (Err problem)
          (Cancelled reason)
          (Indeterminate problem))

        (define (step)
          (println "running")
          nil)

        (define (cognize msg)
          (println msg)
          msg)
    "#;
    assert!(k.eval(core).is_ok(), "agent core load");
    assert!(k.eval("(step)").is_ok(), "step call");
    assert!(k.eval("(cognize \"test\")").is_ok(), "cognize call");
}

#[test]
fn test_repl_arity_mismatch() {
    let mut k = Kernel::new();
    let r = k.eval("(+ 1)");
    assert!(r.is_err(), "arity mismatch should error");
}

#[test]
fn test_repl_syntax_error() {
    let mut k = Kernel::new();
    let r = k.eval("(+ 1 (");
    assert!(r.is_err(), "syntax error should error");
}

#[test]
fn test_repl_system_version() {
    let mut k = Kernel::new();
    assert!(k.eval("(system/version)").is_ok());
}

#[test]
fn test_repl_system_clock() {
    let mut k = Kernel::new();
    assert!(k.eval("(system/clock)").is_ok());
}

#[test]
fn test_repl_sleep_wake() {
    let mut k = Kernel::new();
    let r = k.eval("(sleep 1)");
    assert!(r.is_ok(), "sleep: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::keyword("awake"));

    let r = k.eval(r#"(wake 10000 '(bash "echo hi"))"#);
    assert!(r.is_ok(), "wake: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::keyword("scheduled"));
    assert_eq!(k.wake_timers.len(), 1);
}

#[test]
fn test_tail_call_restores_caller_lexical_frames() {
    let mut k = Kernel::new();
    k.eval("(define n 99)").unwrap();
    k.eval("(define (count n) (if (= n 0) \"done\" (count (- n 1))))")
        .unwrap();
    assert_eq!(k.eval("(count 10000)").unwrap(), Value::string("done"));
    assert_eq!(k.eval("n").unwrap(), Value::Int(99));
    assert_eq!(
        k.eval("(+ 1 ((lambda (x) (* x 2)) 2))").unwrap(),
        Value::Int(5)
    );
}

#[test]
fn test_equal_maps_have_equal_hashes_across_insertion_order() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut k = Kernel::new();
    let first = k.eval("{:a 1 :b 2}").unwrap();
    let second = k.eval("{:b 2 :a 1}").unwrap();
    assert_eq!(first, second);
    let mut a = DefaultHasher::new();
    let mut b = DefaultHasher::new();
    first.hash(&mut a);
    second.hash(&mut b);
    assert_eq!(a.finish(), b.finish());
}

#[test]
fn tail_call_inside_non_tail_function_call_preserves_caller() {
    let mut k = Kernel::new();
    k.eval("(define (g x) (* x 2))").unwrap();
    k.eval("(define (f x) (g x))").unwrap();
    assert_eq!(k.eval("(+ 1 (f 2))").unwrap(), Value::Int(5));
}

#[test]
fn zero_arity_native_rejects_extra_arguments() {
    let mut k = Kernel::new();
    assert!(k.eval("(system/version 1)").is_err());
    assert!(k.eval("(kernel/list 1 2 3)").is_ok());
}

#[test]
fn unicode_character_literal_is_safe() {
    let mut k = Kernel::new();
    assert_eq!(k.eval(r"#\é").unwrap(), Value::String("é".into()));
}

#[test]
fn let_initializers_are_parallel_but_let_star_is_sequential() {
    let mut k = Kernel::new();
    k.eval("(define user/x 10)").unwrap();
    assert_eq!(k.eval("(let ((x 1) (y x)) y)").unwrap(), Value::Int(10));
    assert_eq!(k.eval("(let* ((x 1) (y x)) y)").unwrap(), Value::Int(1));
}

#[test]
fn displayed_values_are_valid_and_informative_lisp() {
    assert_eq!(format!("{}", Value::String("a\"b\n".into())), r#""a\"b\n""#);
    let tagged = Value::Tagged {
        family: "Result".into(),
        variant: "Ok".into(),
        fields: vec![Value::Int(42)],
    };
    assert_eq!(format!("{}", tagged), "(Result/Ok 42)");
}

#[test]
fn binding_history_is_bounded() {
    let mut kernel = Kernel::new();
    for value in 0..100 {
        kernel
            .eval(&format!("(define user/redefined {})", value))
            .unwrap();
    }
    assert_eq!(kernel.env.namespaces["user"].history("redefined").len(), 32);
}

#[test]
fn integer_overflow_is_an_eval_error_not_a_panic() {
    let mut kernel = Kernel::new();
    assert!(kernel.eval("(+ 9223372036854775807 1)").is_err());
}
