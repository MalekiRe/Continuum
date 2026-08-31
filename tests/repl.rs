use persistent_lisp_harness::Value;
use persistent_lisp_harness::{EvalError, Kernel};

// ===== BASIC REPL TESTS =====

#[test]
fn test_repl_define_and_lookup() {
    let mut k = Kernel::new();
    let r = k.eval_value("(define x 42)");
    assert!(r.is_ok(), "define x: {:?}", r.err());
    let r = k.eval_value("x");
    assert!(r.is_ok(), "lookup x: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::Int(42));
}

#[test]
fn test_repl_undefine_and_errors() {
    let mut k = Kernel::new();
    let r = k.eval_value("(define x 42)");
    assert!(r.is_ok());
    let r = k.eval_value("(undefine x)");
    assert!(r.is_ok());
    let r = k.eval_value("x");
    assert!(r.is_err(), "should error on undefined symbol");
}

#[test]
fn test_repl_function_define_and_call() {
    let mut k = Kernel::new();
    let r = k.eval_value("(define (add a b) (+ a b))");
    assert!(r.is_ok());
    let r = k.eval_value("(add 3 4)");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(7));
}

#[test]
fn test_repl_lambda() {
    let mut k = Kernel::new();
    let r = k.eval_value("(define double (lambda (x) (* x 2)))");
    assert!(r.is_ok());
    let r = k.eval_value("(double 5)");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(10));
}

#[test]
fn test_repl_conditional() {
    let mut k = Kernel::new();
    assert_eq!(k.eval_value("(if #t 1 2)").unwrap(), Value::Int(1));
    assert_eq!(k.eval_value("(if #f 1 2)").unwrap(), Value::Int(2));
    assert_eq!(k.eval_value("(if nil 1 2)").unwrap(), Value::Int(2));
    assert_eq!(k.eval_value("(if 42 1 2)").unwrap(), Value::Int(1));
}

#[test]
fn test_repl_let_and_scope() {
    let mut k = Kernel::new();
    let r = k.eval_value("(let ((x 10) (y 20)) (+ x y))");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(30));
    let r = k.eval_value("(let* ((x 1) (y (+ x 1))) y)");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(2));
}

#[test]
fn test_repl_begin() {
    let mut k = Kernel::new();
    let r = k.eval_value("(begin (define a 1) (define b 2) (+ a b))");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(3));
}

#[test]
fn test_repl_set_and_mutation() {
    let mut k = Kernel::new();
    k.eval_value("(define x 10)").unwrap();
    k.eval_value("(set! x 20)").unwrap();
    assert_eq!(k.eval_value("x").unwrap(), Value::Int(20));
}

#[test]
fn test_repl_quote() {
    let mut k = Kernel::new();
    let r = k.eval_value("'(1 2 3)");
    assert!(r.is_ok());
    assert_eq!(
        r.unwrap(),
        Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
    );
}

#[test]
fn test_repl_quasiquote() {
    let mut k = Kernel::new();
    let r = k.eval_value("(let ((x 42)) `(1 ,x 3))");
    assert!(r.is_ok());
    assert_eq!(
        r.unwrap(),
        Value::list(vec![Value::Int(1), Value::Int(42), Value::Int(3)])
    );
}

#[test]
fn test_repl_quasiquote_splicing() {
    let mut k = Kernel::new();
    let r = k.eval_value("(let ((lst '(a b c))) `(x ,@lst y))");
    assert!(r.is_ok());
    assert!(r.unwrap().is_list());
}

#[test]
fn test_repl_list_ops() {
    let mut k = Kernel::new();
    let three = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    assert_eq!(k.eval_value("(list 1 2 3)").unwrap(), three);
    assert_eq!(k.eval_value("(car (list 1 2 3))").unwrap(), Value::Int(1));
    assert_eq!(
        k.eval_value("(cdr (list 1 2 3))").unwrap(),
        Value::list(vec![Value::Int(2), Value::Int(3)])
    );
    assert_eq!(k.eval_value("(cons 1 (list 2 3))").unwrap(), three);
}

#[test]
fn test_repl_arithmetic() {
    let mut k = Kernel::new();
    assert_eq!(k.eval_value("(+ 1 2)").unwrap(), Value::Int(3));
    assert_eq!(k.eval_value("(- 5 3)").unwrap(), Value::Int(2));
    assert_eq!(k.eval_value("(* 2 3)").unwrap(), Value::Int(6));
    assert_eq!(k.eval_value("(= 5 5)").unwrap(), Value::Bool(true));
    assert_eq!(k.eval_value("(= 5 6)").unwrap(), Value::Bool(false));
    assert!(k.eval_value("(< 1 2)").unwrap().is_truthy());
    assert!(!k.eval_value("(> 1 2)").unwrap().is_truthy());
}

#[test]
fn test_repl_string_append() {
    let mut k = Kernel::new();
    let r = k.eval_value(r#"(string-append "hello " "world")"#);
    assert!(r.is_ok(), "string-append: {:?}", r.err());
    let v = r.unwrap();
    assert_eq!(v, Value::string("hello world"), "got {}", v);
}

#[test]
fn test_repl_string_search() {
    let mut k = Kernel::new();
    let r = k.eval_value(r#"(string-search "lisp" "hello lisp world")"#);
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(6));
    let r = k.eval_value(r#"(string-search "xyz" "hello world")"#);
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Bool(false));
}

#[test]
fn test_repl_substring() {
    let mut k = Kernel::new();
    let r = k.eval_value(r#"(substring "hello world" 0 5)"#);
    assert!(r.is_ok(), "substring: {:?}", r.err());
    let v = r.unwrap();
    assert_eq!(v, Value::string("hello"), "got {}", v);
}

#[test]
fn test_repl_type_predicates() {
    let mut k = Kernel::new();
    assert_eq!(k.eval_value("(nil? nil)").unwrap(), Value::Bool(true));
    assert_eq!(k.eval_value("(nil? 42)").unwrap(), Value::Bool(false));
    assert_eq!(k.eval_value("(number? 42)").unwrap(), Value::Bool(true));
    assert_eq!(k.eval_value("(symbol? 'x)").unwrap(), Value::Bool(true));
    assert_eq!(k.eval_value("(string? \"x\")").unwrap(), Value::Bool(true));
    assert_eq!(
        k.eval_value("(list? (list 1 2))").unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        k.eval_value("(function? (lambda (x) x))").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn test_repl_define_data_and_match() {
    let mut k = Kernel::new();
    let r = k.eval_value("(define-data result/Result (Ok value) (Err problem))");
    assert!(r.is_ok());
    let r = k.eval_value(
        r#"(match (result/Result/Ok 42)
          ((result/Result/Ok n) (+ n 1))
          ((result/Result/Err msg) -1))"#,
    );
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(43));
}

#[test]
fn test_repl_macro_syntax_rules() {
    let mut k = Kernel::new();
    let r = k.eval_value(
        r#"
        (define-syntax my-when
          (syntax-rules ()
            ((my-when test body ...)
             (if test (begin body ...) nil))))
    "#,
    );
    assert!(r.is_ok());
    assert_eq!(k.eval_value("(my-when #t 42)").unwrap(), Value::Int(42));
    assert_eq!(k.eval_value("(my-when #f 99)").unwrap(), Value::Nil);
}

#[test]
fn test_repl_nth_and_length() {
    let mut k = Kernel::new();
    assert_eq!(
        k.eval_value("(nth 0 (list 10 20 30))").unwrap(),
        Value::Int(10)
    );
    assert_eq!(
        k.eval_value("(length (list 1 2 3))").unwrap(),
        Value::Int(3)
    );
    assert_eq!(k.eval_value(r#"(length "hello")"#).unwrap(), Value::Int(5));
}

#[test]
fn test_repl_append() {
    let mut k = Kernel::new();
    let r = k.eval_value("(append (list 1 2) (list 3 4))");
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
    k.eval_value("(define (count n) (if (= n 0) \"done\" (count (- n 1))))")
        .unwrap();
    let r = k.eval_value("(count 10000)");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::string("done"));
}

#[test]
fn test_repl_closure() {
    let mut k = Kernel::new();
    let r = k.eval_value("(define (make-adder n) (lambda (x) (+ x n)))");
    assert!(r.is_ok(), "define make-adder: {:?}", r.err());
    let r = k.eval_value("(define add5 (make-adder 5))");
    assert!(r.is_ok(), "define add5: {:?}", r.err());
    let r = k.eval_value("(add5 10)");
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
    assert!(k.eval_value(core).is_ok(), "agent core load");
    assert!(k.eval_value("(step)").is_ok(), "step call");
    assert!(k.eval_value("(cognize \"test\")").is_ok(), "cognize call");
}

#[test]
fn test_repl_arity_mismatch() {
    let mut k = Kernel::new();
    let r = k.eval_value("(+ 1)");
    assert!(r.is_err(), "arity mismatch should error");
}

#[test]
fn test_repl_syntax_error() {
    let mut k = Kernel::new();
    let r = k.eval_value("(+ 1 (");
    assert!(r.is_err(), "syntax error should error");
}

#[test]
fn test_repl_system_version() {
    let mut k = Kernel::new();
    assert!(k.eval_value("(system/version)").is_ok());
}

#[test]
fn test_repl_system_clock() {
    let mut k = Kernel::new();
    assert!(k.eval_value("(system/clock)").is_ok());
}

#[test]
fn test_repl_wake() {
    let mut k = Kernel::new();
    assert!(k.eval_value("(sleep 1)").is_err());

    let r = k.eval_value(r#"(wake 10000 '(bash "echo hi"))"#);
    assert!(r.is_ok(), "wake: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::keyword("scheduled"));
    assert_eq!(k.wake_timer_count(), 1);
}

#[test]
fn test_tail_call_restores_caller_lexical_frames() {
    let mut k = Kernel::new();
    k.eval_value("(define n 99)").unwrap();
    k.eval_value("(define (count n) (if (= n 0) \"done\" (count (- n 1))))")
        .unwrap();
    assert_eq!(
        k.eval_value("(count 10000)").unwrap(),
        Value::string("done")
    );
    assert_eq!(k.eval_value("n").unwrap(), Value::Int(99));
    assert_eq!(
        k.eval_value("(+ 1 ((lambda (x) (* x 2)) 2))").unwrap(),
        Value::Int(5)
    );
}

#[test]
fn test_equal_maps_have_equal_hashes_across_insertion_order() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut k = Kernel::new();
    let first = k.eval_value("{:a 1 :b 2}").unwrap();
    let second = k.eval_value("{:b 2 :a 1}").unwrap();
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
    k.eval_value("(define (g x) (* x 2))").unwrap();
    k.eval_value("(define (f x) (g x))").unwrap();
    assert_eq!(k.eval_value("(+ 1 (f 2))").unwrap(), Value::Int(5));
}

#[test]
fn zero_arity_native_rejects_extra_arguments() {
    let mut k = Kernel::new();
    assert!(k.eval_value("(system/version 1)").is_err());
    assert!(k.eval_value("(kernel/list 1 2 3)").is_ok());
}

#[test]
fn unicode_character_literal_is_safe() {
    let mut k = Kernel::new();
    assert_eq!(k.eval_value(r"#\é").unwrap(), Value::String("é".into()));
}

#[test]
fn let_initializers_are_parallel_but_let_star_is_sequential() {
    let mut k = Kernel::new();
    k.eval_value("(define user/x 10)").unwrap();
    assert_eq!(
        k.eval_value("(let ((x 1) (y x)) y)").unwrap(),
        Value::Int(10)
    );
    assert_eq!(
        k.eval_value("(let* ((x 1) (y x)) y)").unwrap(),
        Value::Int(1)
    );
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
            .eval_value(&format!("(define user/redefined {})", value))
            .unwrap();
    }
    assert_eq!(
        kernel
            .environment()
            .binding_history_len("user", "redefined"),
        32
    );
}

#[test]
fn integer_overflow_is_an_eval_error_not_a_panic() {
    let mut kernel = Kernel::new();
    assert!(kernel.eval_value("(+ 9223372036854775807 1)").is_err());
}

#[test]
fn captured_set_mutates_a_shared_persistent_cell() {
    let mut kernel = Kernel::new();
    kernel
        .eval_value(
            "(define make-counter
               (lambda ()
                 (let ((count 0))
                   (lambda ()
                     (set! count (+ count 1))
                     count))))",
        )
        .unwrap();
    kernel
        .eval_value("(define counter (make-counter))")
        .unwrap();
    assert_eq!(kernel.eval_value("(counter)").unwrap(), Value::Int(1));
    assert_eq!(kernel.eval_value("(counter)").unwrap(), Value::Int(2));
}

#[test]
fn letrec_closures_capture_the_recursive_placeholder_cell() {
    let mut kernel = Kernel::new();
    assert_eq!(
        kernel
            .eval_value(
                "(letrec ((factorial
                            (lambda (n)
                              (if (= n 0)
                                  1
                                  (* n (factorial (- n 1)))))))
                   (factorial 6))",
            )
            .unwrap(),
        Value::Int(720)
    );
}

#[test]
fn calls_bind_parameters_in_a_child_of_the_captured_environment() {
    let mut kernel = Kernel::new();
    kernel
        .eval_value(
            "(define closures
               (let ((x 7))
                 (list (lambda (x) x)
                       (lambda () x))))",
        )
        .unwrap();
    assert_eq!(
        kernel.eval_value("((nth 0 closures) 99)").unwrap(),
        Value::Int(99)
    );
    assert_eq!(
        kernel.eval_value("((nth 1 closures))").unwrap(),
        Value::Int(7)
    );
}

#[test]
fn interpreted_functions_require_exact_arity() {
    let mut kernel = Kernel::new();
    kernel.eval_value("(define (pair x y) (list x y))").unwrap();
    assert!(matches!(
        kernel.eval_value("(pair 1)"),
        Err(EvalError::ArityMismatch {
            expected: 2,
            got: 1,
            ..
        })
    ));
    assert!(matches!(
        kernel.eval_value("(pair 1 2 3)"),
        Err(EvalError::ArityMismatch {
            expected: 2,
            got: 3,
            ..
        })
    ));
    kernel.eval_value("(define (nothing) 42)").unwrap();
    assert!(matches!(
        kernel.eval_value("(nothing 1)"),
        Err(EvalError::ArityMismatch {
            expected: 0,
            got: 1,
            ..
        })
    ));
}

#[test]
fn failed_top_level_form_rolls_back_cells_and_arena_allocations() {
    let mut kernel = Kernel::new();
    kernel
        .eval_value("(define counter (let ((x 0)) (lambda () (set! x (+ x 1)) x)))")
        .unwrap();
    let environments = kernel.lexical_arena_counts().0;
    let cells = kernel.lexical_arena_counts().1;
    assert!(
        kernel
            .eval_value("(begin (counter) (let ((temporary 1)) unknown-symbol))")
            .is_err()
    );
    assert_eq!(kernel.lexical_arena_counts().0, environments);
    assert_eq!(kernel.lexical_arena_counts().1, cells);
    assert_eq!(kernel.eval_value("(counter)").unwrap(), Value::Int(1));
}

#[test]
fn data_families_use_exact_qualified_identity_and_bindings() {
    let mut kernel = Kernel::new();
    kernel
        .eval_value("(define-data alpha/Result (Ok value) (Old value))")
        .unwrap();
    kernel
        .eval_value("(define-data beta/Result (Ok value))")
        .unwrap();
    kernel
        .eval_value("(define alpha/Result/unrelated 17)")
        .unwrap();

    assert!(kernel.eval_value("(alpha/Result/Ok 1)").is_ok());
    assert!(kernel.eval_value("(beta/Result/Ok 2)").is_ok());
    assert_eq!(
        kernel
            .eval_value("(match (beta/Result/Ok 2) ((alpha/Result/Ok x) 0) ((beta/Result/Ok x) x))")
            .unwrap(),
        Value::Int(2)
    );

    kernel.eval_value("(undefine alpha/Result)").unwrap();
    assert!(kernel.eval_value("(alpha/Result/Ok 1)").is_err());
    assert!(kernel.eval_value("(alpha/Result/Old 1)").is_err());
    assert_eq!(
        kernel.eval_value("alpha/Result/unrelated").unwrap(),
        Value::Int(17)
    );
    assert!(kernel.eval_value("(beta/Result/Ok 2)").is_ok());
}

#[test]
fn redefining_a_data_family_removes_only_its_stale_constructors() {
    let mut kernel = Kernel::new();
    kernel
        .eval_value("(define-data shapes/Shape (Circle radius) (Square side))")
        .unwrap();
    kernel
        .eval_value("(define-data shapes/Shape (Circle radius))")
        .unwrap();
    assert!(kernel.eval_value("(shapes/Shape/Square 2)").is_err());
    assert!(kernel.eval_value("(shapes/Shape/Circle 2)").is_ok());
}

#[test]
fn output_sinks_are_kernel_local() {
    use persistent_lisp_harness::OutputSink;
    use std::sync::{Arc, Mutex};
    let left_output = Arc::new(Mutex::new(String::new()));
    let right_output = Arc::new(Mutex::new(String::new()));
    let mut left = Kernel::new();
    let mut right = Kernel::new();
    let captured = left_output.clone();
    left.set_output_sink(OutputSink::new(move |text| {
        captured.lock().unwrap().push_str(text)
    }));
    let captured = right_output.clone();
    right.set_output_sink(OutputSink::new(move |text| {
        captured.lock().unwrap().push_str(text)
    }));
    left.eval_value(r#"(display "left")"#).unwrap();
    right.eval_value(r#"(println "right")"#).unwrap();
    assert_eq!(&*left_output.lock().unwrap(), r#""left""#);
    assert_eq!(&*right_output.lock().unwrap(), "\"right\"\n");
}

#[test]
fn eval_cancellation_is_kernel_local() {
    use std::sync::mpsc;
    use std::time::Duration;
    let mut interrupted = Kernel::new();
    let interrupt = interrupted.eval_interrupt_handle();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = interrupted.eval_value("(begin (define (loop n) (loop (+ n 1))) (loop 0))");
        let _ = sender.send(result.map_err(|error| error.to_string()));
    });
    for _ in 0..1_000 {
        if interrupt.is_running() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(interrupt.is_running());
    assert!(interrupt.request_interrupt());
    let result = receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("evaluation did not stop");
    assert!(result.unwrap_err().contains("interrupted"));

    let mut independent = Kernel::new();
    assert_eq!(independent.eval_value("(+ 1 2)").unwrap(), Value::Int(3));
}

#[test]
fn eval_interrupt_queued_before_activation_cancels_the_imminent_eval() {
    let mut kernel = Kernel::new();
    let interrupt = kernel.eval_interrupt_handle();
    assert!(!interrupt.request_interrupt());
    assert!(matches!(
        kernel.eval_value("(+ 1 2)"),
        Err(persistent_lisp_harness::EvalError::Interrupted)
    ));
    assert_eq!(kernel.eval_value("(+ 1 2)").unwrap(), Value::Int(3));
}

#[test]
fn interrupt_at_eval_completion_discards_the_completed_value() {
    use persistent_lisp_harness::OutputSink;
    let mut kernel = Kernel::new();
    let interrupt = kernel.eval_interrupt_handle();
    let callback = interrupt.clone();
    kernel.set_output_sink(OutputSink::new(move |_| {
        assert!(callback.request_interrupt());
    }));
    assert!(matches!(
        kernel.eval_value(r#"(display "finishing")"#),
        Err(persistent_lisp_harness::EvalError::Interrupted)
    ));
}

#[test]
fn direct_eval_value_restores_scope_after_failing_letrec() {
    use persistent_lisp_harness::vm::{eval::eval_value, reader::read_one};
    let mut kernel = persistent_lisp_harness::Kernel::new();
    let (form, _) = read_one("(letrec ((x missing)) 123)").unwrap();
    assert!(eval_value(form, &mut kernel).is_err());
    assert!(matches!(
        eval_value(persistent_lisp_harness::Value::symbol("x"), &mut kernel),
        Err(persistent_lisp_harness::EvalError::UndefinedSymbol(name)) if name == "x"
    ));
}

#[test]
fn rhs_tail_calls_restore_the_enclosing_lexical_scope() {
    let mut kernel = Kernel::new();
    kernel.eval_value("(define (id x) x)").unwrap();
    assert_eq!(
        kernel
            .eval_value("(let ((a 1)) (begin (set! a (id 2)) a))")
            .unwrap(),
        Value::Int(2)
    );
    assert_eq!(
        kernel
            .eval_value("(let ((a 3)) (begin (define user/from-local (id a)) a))")
            .unwrap(),
        Value::Int(3)
    );
}

#[test]
fn registered_native_bindings_are_immutable() {
    let mut kernel = Kernel::new();
    for name in [
        "agent/call",
        "message/reply",
        "model/call",
        "memory/remember",
        "source/get",
    ] {
        assert!(kernel.eval_value(&format!("(define {name} 1)")).is_err());
        assert!(kernel.eval_value(&format!("(undefine {name})")).is_err());
        assert_eq!(
            kernel.eval_value(&format!("(function? {name})")).unwrap(),
            Value::Bool(true)
        );
    }
}

#[test]
fn top_level_bash_returns_a_typed_trap() {
    let mut kernel = Kernel::new();
    let outcome = kernel.eval(r#"(bash "echo hello world")"#).unwrap();
    assert!(matches!(
        outcome,
        persistent_lisp_harness::EvalOutcome::Trap(
            persistent_lisp_harness::TrapRequest {
                operation: persistent_lisp_harness::VmTrap::RunBash { ref command },
                ..
            }
        ) if command == "echo hello world"
    ));
}

#[test]
fn immutable_prelude_bindings_do_not_lock_their_namespaces() {
    let mut kernel = Kernel::new();
    assert!(kernel.eval_value("(define agent/call 1)").is_err());
    assert!(kernel.eval_value("(undefine agent/call)").is_err());
    kernel.eval_value("(define agent/helper 42)").unwrap();
    assert_eq!(kernel.eval_value("agent/helper").unwrap(), Value::Int(42));
    assert!(
        kernel
            .environment()
            .source("agent/call")
            .is_some_and(|source| source.contains("kernel/trap"))
    );
}

#[test]
fn set_mutates_state_without_creating_definition_history() {
    let mut kernel = Kernel::new();
    kernel.eval_value("(define counter 0)").unwrap();
    let versions = kernel.environment().binding_history_len("user", "counter");
    kernel.eval_value("(set! counter 1)").unwrap();
    assert_eq!(
        kernel.environment().binding_history_len("user", "counter"),
        versions
    );
    assert_eq!(kernel.eval_value("counter").unwrap(), Value::Int(1));
}

#[test]
fn printed_vectors_and_integral_floats_round_trip() {
    let mut kernel = Kernel::new();
    for source in ["[1 2 3]", "1.0", "[1.0 :x \"y\"]"] {
        let value = kernel.eval_value(source).unwrap();
        assert_eq!(kernel.eval_value(&value.to_string()).unwrap(), value);
    }
    assert!(kernel.eval_value("1e9999").is_err());
}

#[test]
fn traps_are_only_allowed_as_the_final_action() {
    let mut kernel = Kernel::new();
    assert!(kernel.eval("(define x (bash \"true\"))").is_err());
    assert!(kernel.eval("(set! x (bash \"true\"))").is_err());
    assert!(kernel.eval("(bash \"true\") (define x 1)").is_err());
    assert!(matches!(
        kernel.eval("(match :run (:run (bash \"true\")))").unwrap(),
        persistent_lisp_harness::EvalOutcome::Trap(_)
    ));
}

#[test]
fn multi_expression_tail_recursion_is_constant_stack() {
    let mut kernel = Kernel::new();
    kernel
        .eval_value("(define (count n) nil (if (= n 0) n (count (- n 1))))")
        .unwrap();
    assert_eq!(kernel.eval_value("(count 50000)").unwrap(), Value::Int(0));
}

#[test]
fn lisp_evaluation_can_pause_and_resume_without_unwinding() {
    use std::sync::mpsc;
    use std::time::Duration;
    let mut kernel = Kernel::new();
    let handle = kernel.eval_interrupt_handle();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let result = kernel
            .eval_value("(begin (define (loop n) (if (= n 100000) n (loop (+ n 1)))) (loop 0))");
        send.send(result).unwrap();
    });
    while !handle.is_running() {
        std::thread::yield_now();
    }
    assert!(handle.request_pause());
    assert!(handle.wait_until_paused(Duration::from_secs(2)));
    assert!(receive.recv_timeout(Duration::from_millis(20)).is_err());
    handle.resume();
    assert_eq!(
        receive
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap(),
        Value::Int(100_000)
    );
}

#[test]
fn hooks_wrap_native_and_interpreted_named_calls_in_order() {
    let mut kernel = Kernel::new();
    kernel
        .eval_value(
            r#"
            (define hook/log "")
            (define (hook/before target args)
              (set! hook/log (string-append hook/log "B")))
            (define (hook/after target args result)
              (set! hook/log (string-append hook/log "A")))
            (define (twice x) (* x 2))
            (hook/add "before-plus" '+ :before 'hook/before)
            (hook/add "after-plus" '+ :after 'hook/after)
            (hook/add "before-twice" 'twice :before 'hook/before)
            (hook/add "after-twice" 'twice :after 'hook/after)
            "#,
        )
        .unwrap();
    assert_eq!(kernel.eval_value("(+ 1 2)").unwrap(), Value::Int(3));
    assert_eq!(kernel.eval_value("(twice 4)").unwrap(), Value::Int(8));
    assert_eq!(
        kernel.eval_value("hook/log").unwrap(),
        Value::string("BABA")
    );
}

#[test]
fn hook_self_recursion_is_suppressed_and_faults_roll_back_everything() {
    let mut kernel = Kernel::new();
    kernel
        .eval_value(
            r#"
            (define count 0)
            (define (recursive target args)
              (set! count (+ count 1)))
            (hook/add "recursive" '+ :before 'recursive)
            "#,
        )
        .unwrap();
    assert_eq!(kernel.eval_value("(+ 1 2)").unwrap(), Value::Int(3));
    assert_eq!(kernel.eval_value("count").unwrap(), Value::Int(1));

    kernel
        .eval_value(
            r#"
            (define stable 0)
            (define (broken target args)
              (set! stable 99)
              (memory/note "should roll back")
              (context/inject "bad" :frame "bad")
              (wake 1000 "bad")
              (kernel/error "hook failed"))
            (hook/add "broken" '* :before 'broken)
            "#,
        )
        .unwrap();
    assert!(
        kernel
            .eval_value("(begin (define ghost 1) (* 2 3))")
            .is_err()
    );
    assert_eq!(kernel.eval_value("stable").unwrap(), Value::Int(0));
    assert!(kernel.eval_value("ghost").is_err());
    assert_eq!(kernel.wake_timer_count(), 0);
    assert_eq!(
        kernel
            .eval_value("(memory/recall \"should roll back\")")
            .unwrap(),
        Value::List(Vec::new())
    );
    assert_eq!(
        kernel.eval_value("(context/list)").unwrap(),
        Value::List(Vec::new())
    );
    assert_eq!(
        kernel
            .eval_value("(history/find \"should roll back\")")
            .unwrap(),
        Value::List(Vec::new())
    );
}

#[test]
fn after_hook_runs_before_a_final_trap_is_scheduled() {
    let mut kernel = Kernel::new();
    kernel
        .eval_value(
            r#"
            (define (after-bash target args result)
              (memory/remember "bash-result" result))
            (hook/add "after-bash" 'bash :after 'after-bash)
            "#,
        )
        .unwrap();
    assert!(matches!(
        kernel.eval("(bash \"true\")").unwrap(),
        persistent_lisp_harness::EvalOutcome::Trap(_)
    ));
    assert!(
        kernel
            .eval_value("(memory/recall \"scheduled\")")
            .unwrap()
            .to_string()
            .contains("scheduled")
    );
}
