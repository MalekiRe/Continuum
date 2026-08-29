
use persistent_lisp_harness::Kernel;
use persistent_lisp_harness::Value;
use std::time::Instant;

fn make_kernel() -> Kernel {
    let mut k = Kernel::new();
    k.register_tools();
    k
}

#[test]
fn bench_10000_defines() {
    let mut k = make_kernel();
    let start = Instant::now();
    for i in 0..10000 {
        let expr = format!("(define (fn{0} x) (+ x {0}))", i);
        k.eval(&expr).unwrap();
    }
    let elapsed = start.elapsed();
    let per_def = elapsed.as_micros() / 10000;
    println!("10000 defines: {}ms ({}µs per define)", elapsed.as_millis(), per_def);

    // Call the last one
    let start = Instant::now();
    for _ in 0..1000 {
        k.eval("(fn9999 1)").unwrap();
    }
    let elapsed = start.elapsed();
    let per_call = elapsed.as_micros() / 1000;
    println!("1000 calls after 10000 defs: {}ms ({}µs per call)", elapsed.as_millis(), per_call);
}

#[test]
fn bench_50000_defines() {
    let mut k = make_kernel();
    let start = Instant::now();
    for i in 0..50000 {
        let expr = format!("(define (fn{0} x) (+ x {0}))", i);
        k.eval(&expr).unwrap();
    }
    let elapsed = start.elapsed();
    let per_def = elapsed.as_micros() / 50000;
    println!("50000 defines: {}ms ({}µs per define)", elapsed.as_millis(), per_def);

    // Call the last one
    let start = Instant::now();
    for _ in 0..1000 {
        k.eval("(fn49999 1)").unwrap();
    }
    let elapsed = start.elapsed();
    let per_call = elapsed.as_micros() / 1000;
    println!("1000 calls after 50000 defs: {}ms ({}µs per call)", elapsed.as_millis(), per_call);
}

#[test]
fn bench_deeply_nested_calls() {
    let mut k = make_kernel();

    // Build a chain of 100 functions: f0 calls f1, f1 calls f2, ...
    for i in 0..100 {
        if i == 99 {
            k.eval(&format!("(define (fn{} x) x)", i)).unwrap();
        } else {
            k.eval(&format!("(define (fn{} x) (fn{} x))", i, i+1)).unwrap();
        }
    }
    println!("Defined 100-deep call chain");

    let start = Instant::now();
    let r = k.eval("(fn0 42)");
    let elapsed = start.elapsed();
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(42));
    println!("100-deep call chain: {}µs", elapsed.as_micros());
}

#[test]
fn bench_mixed_workload() {
    let mut k = make_kernel();

    // Agent-like workload: define tools, then use them in a loop
    k.eval(r#"
        (define (add a b) (+ a b))
        (define (sub a b) (- a b))
        (define (mul a b) (* a b))
        (define (square x) (mul x x))
        (define (sum-squares a b) (add (square a) (square b)))
    "#).unwrap();

    let start = Instant::now();
    for _ in 0..10000 {
        k.eval("(sum-squares 3 4)").unwrap();
    }
    let elapsed = start.elapsed();
    let per_call = elapsed.as_micros() / 10000;
    println!("10000 mixed calls: {}ms ({}µs per call)", elapsed.as_millis(), per_call);
}

#[test]
fn bench_define_and_call_inline() {
    let mut k = make_kernel();

    let start = Instant::now();
    for i in 0..10000 {
        k.eval(&format!("(define (f{} x) (+ x 1)) (f{} 42)", i, i)).unwrap();
    }
    let elapsed = start.elapsed();
    let per_pair = elapsed.as_micros() / 10000;
    println!("10000 define+call pairs: {}ms ({}µs per pair)", elapsed.as_millis(), per_pair);
}

#[test]
fn bench_closure_capture_cost() {
    let mut k = make_kernel();

    // Define in a large environment
    for i in 0..1000 {
        k.eval(&format!("(define (dummy{} x) x)", i)).unwrap();
    }

    let start = Instant::now();
    for i in 0..1000 {
        k.eval(&format!("(define (f{} x) (+ x 1)) (f{} 42)", i, i)).unwrap();
    }
    let elapsed = start.elapsed();
    let per_pair = elapsed.as_micros() / 1000;
    println!("1000 define+call in 1000-def env: {}ms ({}µs per pair)", elapsed.as_millis(), per_pair);
}

#[test]
fn bench_tail_recursion_10million() {
    let mut k = make_kernel();
    k.eval(r#"(define (count n) (if (= n 0) "done" (count (- n 1))))"#).unwrap();

    let start = Instant::now();
    let r = k.eval("(count 10000000)");
    let elapsed = start.elapsed();
    assert!(r.is_ok(), "10M tail recursion: {:?}", r.err());
    let per_iter = elapsed.as_micros() / 10000000;
    println!("10M tail recursion: {}ms ({}ns per call)", elapsed.as_millis(), per_iter * 1000);
}
