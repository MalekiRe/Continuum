use persistent_lisp_harness::vm::reader;
use persistent_lisp_harness::{Kernel, Value};
use std::panic::{AssertUnwindSafe, catch_unwind};

#[test]
fn substring_uses_unicode_scalar_indices() {
    let mut kernel = Kernel::new();
    assert_eq!(
        kernel.eval_value(r#"(substring "aé🦀z" 1 3)"#).unwrap(),
        Value::string("é🦀")
    );
    assert_eq!(
        kernel.eval_value(r#"(substring "aé🦀z" 4 4)"#).unwrap(),
        Value::string("")
    );
    assert_eq!(
        kernel
            .eval_value(r#"(string-search "🦀" "aé🦀z")"#)
            .unwrap(),
        Value::Int(2)
    );
    assert_eq!(
        kernel.eval_value(r#"(length "aé🦀z")"#).unwrap(),
        Value::Int(4)
    );
}

#[test]
fn substring_rejects_invalid_indices_instead_of_clamping_or_panicking() {
    let mut kernel = Kernel::new();
    for form in [
        r#"(substring "é" -1 1)"#,
        r#"(substring "é" 0 -1)"#,
        r#"(substring "é" 1 0)"#,
        r#"(substring "é" 0 2)"#,
        r#"(substring "é" 0 1.5)"#,
    ] {
        assert!(
            kernel.eval_value(form).is_err(),
            "accepted invalid form: {form}"
        );
    }
}

#[test]
fn collection_indices_and_wake_durations_must_be_non_negative() {
    let mut kernel = Kernel::new();
    assert!(kernel.eval_value("(nth -1 '(a b))").is_err());
    assert!(kernel.eval_value("(vector/get #(a b) -1)").is_err());
    assert!(kernel.eval_value(r#"(wake -1 "never")"#).is_err());
    assert_eq!(kernel.wake_timer_count(), 0);
}

#[test]
fn blocking_stdin_and_sleep_natives_are_not_registered() {
    let mut kernel = Kernel::new();
    assert!(kernel.eval_value("(read)").is_err());
    assert!(kernel.eval_value("(sleep 1)").is_err());
    assert_eq!(
        kernel.eval_value(r#"(wake 0 "now")"#).unwrap(),
        Value::keyword("scheduled")
    );
}

#[test]
fn odd_map_literals_are_rejected_by_reader_and_evaluator() {
    assert!(reader::read_all("{:a 1 :b}").is_err());
    assert!(reader::read_all("'{:a}").is_err());
    let mut kernel = Kernel::new();
    assert!(kernel.eval_value("{:a 1 :b}").is_err());
    assert!(kernel.eval_value("'{:a}").is_err());
}

#[test]
fn interpreted_functions_enforce_arity() {
    let mut kernel = Kernel::new();
    kernel.eval_value("(define (pair a b) (list a b))").unwrap();
    assert!(kernel.eval_value("(pair 1)").is_err());
    assert!(kernel.eval_value("(pair 1 2 3)").is_err());
}

#[test]
fn value_accessors_are_typed_and_non_coercing() {
    assert_eq!(Value::Int(7).as_int(), Some(7));
    assert_eq!(Value::Float(1.5).as_number(), Some(1.5));
    assert_eq!(Value::string("x").as_str(), Some("x"));
    assert_eq!(Value::symbol("x").as_symbol(), Some("x"));
    assert!(Value::Int(7).as_str().is_none());
    assert!(Value::Nil.as_list().is_none());
    assert!(Value::Int(-1).require_nonnegative_usize("test", 1).is_err());
    assert!(Value::Int(7).require_string("test", 1).is_err());
}

#[test]
fn arbitrary_unicode_input_never_panics_in_reader_or_evaluator() {
    // A deterministic, property-like corpus containing ASCII syntax and varied
    // one- to four-byte Unicode scalars. Rust strings are valid UTF-8 by construction.
    const ALPHABET: &[char] = &[
        '(', ')', '[', ']', '{', '}', '\'', '`', ',', '#', ':', ';', '"', '\\', ' ', '\n', '\t',
        'a', 'Z', '0', '9', '-', '+', '.', '/', 'é', 'λ', '中', '🦀', '\u{2003}', '\u{0301}', '\0',
    ];
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;

    for case in 0..512 {
        let len = case % 80;
        let mut input = String::new();
        for _ in 0..len {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            input.push(ALPHABET[(state as usize) % ALPHABET.len()]);
        }

        let read = catch_unwind(|| reader::read_all(&input));
        assert!(read.is_ok(), "reader panicked for {input:?}");

        let evaluated = catch_unwind(AssertUnwindSafe(|| {
            let mut kernel = Kernel::new();
            let _ = kernel.eval_value(&input);
        }));
        assert!(evaluated.is_ok(), "evaluator panicked for {input:?}");
    }
}

#[test]
fn qualified_bindings_require_nonempty_namespace_and_name() {
    let mut kernel = Kernel::new();
    assert!(kernel.eval_value("(define /missing-namespace 1)").is_err());
    assert!(kernel.eval_value("(define missing-name/ 1)").is_err());
}

#[test]
fn deeply_prefixed_forms_are_read_without_reader_recursion() {
    let source = format!("{}nil", "'".repeat(20_000));
    let (value, rest) = reader::read_one(&source).unwrap();
    assert!(rest.is_empty());
    std::mem::forget(value); // Recursive Value drop is separate from reader stack safety.
}
