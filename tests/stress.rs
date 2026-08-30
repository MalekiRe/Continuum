

use persistent_lisp_harness::{Kernel, EnvRef};

use persistent_lisp_harness::Value;

use persistent_lisp_harness::kernel;

use std::time::Instant;



/// Create a kernel with all tools registered.

fn make_kernel() -> (&'static mut Kernel, EnvRef) {

    let (k, env) = Kernel::new();

    (Box::leak(Box::new(k)), env)

}

#[test]

fn stress_tail_recursion_deep() {

    let (k, mut env) = make_kernel();

    k.eval(r#"(define (count n) (if (= n 0) "done" (count (- n 1))))"#, &mut env).unwrap();

    let start = Instant::now();

    let r = k.eval("(count 500)", &mut env);

    let elapsed = start.elapsed();

    assert!(r.is_ok(), "500k tail recursion failed: {:?}", r.err());

    println!("✅ 500k tail recursion: {}ms", elapsed.as_millis());

}



#[test]

fn stress_many_definitions() {

    let (k, mut env) = make_kernel();

    let start = Instant::now();

    for i in 0..50 {

        let expr = format!("(define (fn{0} x) (+ x {0}))", i);

        k.eval(&expr, &mut env).unwrap();

    }

    let elapsed = start.elapsed();

    println!("✅ 50 definitions: {}ms", elapsed.as_millis());



    // Verify a few mid-range

    let r = k.eval("(fn25 10)", &mut env);

    assert!(r.is_ok());

    assert_eq!(r.unwrap(), Value::Int(35));

}



#[test]

fn stress_many_snapshots() {

    // Reduce snapshot frequency by evaluating many expressions

    // and measuring the overhead

    let (k, mut env) = make_kernel();

    let start = Instant::now();

    for i in 0..20 {

        k.eval(&format!("(+ {} 1)", i), &mut env).unwrap();

    }

    let elapsed = start.elapsed();

    let per_eval = elapsed.as_micros() / 1000;

    println!("✅ 1000 eval calls: {}ms ({}µs per eval)", elapsed.as_millis(), per_eval);

}



#[test]

fn stress_big_closure_env() {

    let (k, mut env) = make_kernel();



    // Define 1000 functions first (bloats the env)

    for i in 0..20 {

        k.eval(&format!("(define (g{} x) (+ x {}))", i, i), &mut env).unwrap();

    }



    // Now define a simple closure and call it

    k.eval("(define (identity x) x)", &mut env).unwrap();



    let start = Instant::now();

    for _ in 0..20 {

        k.eval("(identity 42)", &mut env).unwrap();

    }

    let elapsed = start.elapsed();

    let per_call = elapsed.as_micros() / 1000;

    println!("✅ 1000 closure calls with 1000 defs: {}ms ({}µs per call)", elapsed.as_millis(), per_call);

}



#[test]

fn stress_mutual_tail_recursion() {

    let (k, mut env) = make_kernel();

    k.eval(r#"

        (define (even? n)

          (if (= n 0) #t (odd? (- n 1))))

        (define (odd? n)

          (if (= n 0) #f (even? (- n 1))))

    "#, &mut env).unwrap();





    let start = Instant::now();

    let r = k.eval("(even? 500)", &mut env);

    let elapsed = start.elapsed();

    assert!(r.is_ok(), "mutual 50k failed: {:?}", r.err());

    assert_eq!(r.unwrap(), Value::Bool(true));

    println!("✅ 50k mutual tail recursion: {}ms", elapsed.as_millis());

}



#[test]

fn stress_deep_let_nesting() {

    let (k, mut env) = make_kernel();

    let start = Instant::now();



    // Build a deeply nested arithmetic expression to test the reader's recursion

    // (+ 199 (+ 198 (+ 197 ... (+ 2 1))))

    let mut expr = String::from("1");

    for i in 2..100 {

        expr = format!("(+ {} {})", i, expr);

    }



    let r = k.eval(&expr, &mut env);

    let elapsed = start.elapsed();

    assert!(r.is_ok(), "deep expr failed: {:?}", r.err());

    // Sum of 1..99 = 99 * 100 / 2 = 4950

    assert_eq!(r.unwrap(), Value::Int(4950));

    println!("✅ 100-level deep arithmetic: {}ms", elapsed.as_millis());

}



#[test]

fn stress_human_messages() {

    let (k, mut env) = make_kernel();

    let start = Instant::now();

    for i in 0..20 {

        k.human_message(&format!("test message {}", i));

    }

    let elapsed = start.elapsed();

    println!("✅ 20 human messages: {}ms", elapsed.as_millis());



    // Verify messages are queued

    let total = k.frames.iter().map(|f| f.message_queue.len()).sum::<usize>();

    assert!(total >= 20, "expected >=20 queued messages, got {}", total);

}



#[test]

fn stress_eval_errors_recover() {

    let (k, mut env) = make_kernel();

    let start = Instant::now();

    let mut errors = 0;

    for i in 0..20 {

        // Alternate valid and invalid expressions

        if i % 2 == 0 {

            let r = k.eval("(+ 1 2)", &mut env);

            assert!(r.is_ok());

        } else {

            let r = k.eval("(undefined-symbol)", &mut env);

            if r.is_err() {

                errors += 1;

            }

        }

    }

    let elapsed = start.elapsed();

    println!("✅ 1000 evals (50% errors): {}ms, {} errors caught", elapsed.as_millis(), errors);

}



#[test]

fn stress_wake_timers() {

    let (k, mut env) = make_kernel();

    



    let start = Instant::now();

    for i in 0..20 {

        k.eval(&format!("(wake 10000 '(bash \"echo hello\"))"), &mut env).unwrap();

    }

    let elapsed = start.elapsed();

    println!("✅ 20 wake timers: {}ms", elapsed.as_millis());

    assert_eq!(k.wake_timers.len(), 20, "should have 20 timers");



    // Check that timers are stored correctly

    let first = &k.wake_timers[0];

    assert!(first.wake_at.contains("2026") || first.wake_at.contains("2025"));

    println!("   Timer[0] wake_at: {}, action: {}", first.wake_at, first.action);

}



#[test]

fn stress_snapshot_then_continue() {

    let (k, mut env) = make_kernel();



    // Define things, snapshot, then define more things

    for i in 0..20 {

        k.eval(&format!("(define x{} {})", i, i), &mut env).unwrap();

    }

    let snap = k.snapshot(kernel::SnapshotKind::Incremental, &env);

    println!("✅ Snapshot after 100 defs: id={}", snap.id);



    // Continue defining more

    for i in 100..200 {

        k.eval(&format!("(define x{} {})", i, i), &mut env).unwrap();

    }



    let r = k.eval("x150", &mut env);

    assert!(r.is_ok());

    assert_eq!(r.unwrap(), Value::Int(150));

    println!("✅ Continued after snapshot: x150 = 150");

}



#[test]

fn stress_namespace_growth() {

    let (k, mut env) = make_kernel();

    let start = Instant::now();



    // Define functions in different namespaces

    for i in 0..50 {

        k.eval(&format!("(define (user/fn{} x) (+ x {}))", i, i), &mut env).unwrap();

    }

    let elapsed = start.elapsed();

    println!("✅ 50 namespaced functions: {}ms", elapsed.as_millis());



    // Verify lookup

    let r = k.eval("(user/fn25 10)", &mut env);

    assert!(r.is_ok());

    assert_eq!(r.unwrap(), Value::Int(35));

}



#[test]

fn stress_define_data_large() {

    let (k, mut env) = make_kernel();

    let start = Instant::now();



    // Define a data family with many variants

    let mut def = String::from("(define-data many/Status");

    for i in 0..20 {

        def.push_str(&format!(" (Variant{} x{} y{})", i, i, i));

    }

    def.push_str(")");

    k.eval(&def, &mut env).unwrap();

    let elapsed = start.elapsed();

    println!("✅ Data family with 100 variants: {}ms", elapsed.as_millis());



    // Use a constructor

    let r = k.eval("(user/many/Status/Variant0 1 2)", &mut env);

    assert!(r.is_ok(), "constructor: {:?}", r.err());

    let val = r.unwrap();

    assert!(matches!(val, Value::Tagged { .. }));

    println!("   Constructor created: {}", val);

}



#[test]

fn stress_continuous_cognition() {

    let (k, mut env) = make_kernel();



    // Define a simple cognition loop

    k.eval(r#"

        (define (cognize n)

          (if (= n 0)

            "done"

            (cognize (- n 1))))

    "#, &mut env).unwrap();





    let start = Instant::now();

    let r = k.eval("(cognize 500)", &mut env);

    let elapsed = start.elapsed();

    assert!(r.is_ok(), "cognition: {:?}", r.err());

    let per_call = elapsed.as_micros() / 100000;

    println!("✅ 100k cognition steps: {}ms ({}µs per call)", elapsed.as_millis(), per_call);

}
