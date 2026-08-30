use persistent_lisp_harness::vm::reader;
use persistent_lisp_harness::{Kernel, Value};
use std::panic::{AssertUnwindSafe, catch_unwind};

#[test]
fn substring_uses_unicode_scalar_indices() {
    let mut kernel = Kernel::new();
    assert_eq!(
        kernel.eval(r#"(substring "aé🦀z" 1 3)"#).unwrap(),
        Value::string("é🦀")
    );
    assert_eq!(
        kernel.eval(r#"(substring "aé🦀z" 4 4)"#).unwrap(),
        Value::string("")
    );
    assert_eq!(
        kernel.eval(r#"(string-search "🦀" "aé🦀z")"#).unwrap(),
        Value::Int(2)
    );
    assert_eq!(kernel.eval(r#"(length "aé🦀z")"#).unwrap(), Value::Int(4));
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
        assert!(kernel.eval(form).is_err(), "accepted invalid form: {form}");
    }
}

#[test]
fn collection_indices_and_wake_durations_must_be_non_negative() {
    let mut kernel = Kernel::new();
    assert!(kernel.eval("(nth -1 '(a b))").is_err());
    assert!(kernel.eval("(vector/get #(a b) -1)").is_err());
    assert!(kernel.eval(r#"(wake -1 "never")"#).is_err());
    assert!(kernel.wake_timers.is_empty());
}

#[test]
fn blocking_stdin_and_sleep_natives_are_not_registered() {
    let mut kernel = Kernel::new();
    assert!(kernel.eval("(read)").is_err());
    assert!(kernel.eval("(sleep 1)").is_err());
    assert_eq!(
        kernel.eval(r#"(wake 0 "now")"#).unwrap(),
        Value::keyword("scheduled")
    );
}

#[test]
fn retired_blocking_natives_are_removed_during_recovery() {
    let directory = std::env::temp_dir().join(format!(
        "continuum-retired-natives-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&directory).unwrap();

    let mut kernel = Kernel::new();
    kernel.storage.snapshot_dir = directory.to_string_lossy().into_owned();
    kernel.env.force_define("kernel/read", Value::Int(1));
    kernel.env.force_define("kernel/sleep", Value::Int(1));
    kernel.snapshot().unwrap();

    let recovered = Kernel::recover_from_dir(&directory).unwrap();
    assert!(recovered.env.lookup("kernel/read").is_none());
    assert!(recovered.env.lookup("kernel/sleep").is_none());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn odd_map_literals_are_rejected_by_reader_and_evaluator() {
    assert!(reader::read_all("{:a 1 :b}").is_err());
    assert!(reader::read_all("'{:a}").is_err());
    let mut kernel = Kernel::new();
    assert!(kernel.eval("{:a 1 :b}").is_err());
    assert!(kernel.eval("'{:a}").is_err());
}

#[test]
fn interpreted_functions_enforce_arity() {
    let mut kernel = Kernel::new();
    kernel.eval("(define (pair a b) (list a b))").unwrap();
    assert!(kernel.eval("(pair 1)").is_err());
    assert!(kernel.eval("(pair 1 2 3)").is_err());
}

#[test]
fn value_accessors_are_typed_and_non_coercing() {
    assert_eq!(Value::Int(7).as_int(), Some(7));
    assert_eq!(Value::Float(1.5).as_number(), Some(1.5));
    assert_eq!(Value::string("x").as_str(), Some("x"));
    assert_eq!(Value::symbol("x").as_symbol(), Some("x"));
    assert!(Value::Int(7).as_str().is_none());
    assert!(Value::Nil.as_list().is_none());
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
            let _ = kernel.eval(&input);
        }));
        assert!(evaluated.is_ok(), "evaluator panicked for {input:?}");
    }
}
