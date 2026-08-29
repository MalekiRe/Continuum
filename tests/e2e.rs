
use persistent_lisp_harness::Kernel;
use persistent_lisp_harness::Value;
use persistent_lisp_harness::kernel::SnapshotKind;

fn make_kernel() -> Kernel {
    let mut k = Kernel::new();
    k.register_tools();
    k
}

#[test]
fn test_snapshot_roundtrip_direct() {
    // Test snapshot by directly serializing/deserializing the kernel,
    // bypassing the file-based recovery
    let mut k = make_kernel();

    k.eval("(define x 100)").unwrap();
    k.eval("(define y 200)").unwrap();

    // Serialize directly
    let json = serde_json::to_string(&k).unwrap();
    println!("Kernel JSON: {} bytes", json.len());

    // Deserialize
    let mut recovered: Kernel = serde_json::from_str(&json).unwrap();
    recovered.register_natives();

    let rx = recovered.eval("x").unwrap();
    let ry = recovered.eval("y").unwrap();
    assert_eq!(rx, Value::Int(100));
    assert_eq!(ry, Value::Int(200));
    println!("Direct roundtrip: x={}, y={}", rx, ry);
}

#[test]
fn test_snapshot_file_recovery() {
    // Clean up old snapshots
    let _ = std::fs::remove_dir_all("snapshots");

    let mut k = make_kernel();

    k.eval("(define x 100)").unwrap();
    k.eval("(define y 200)").unwrap();

    // Take a single full snapshot (bypassing eval's auto-snapshots)
    let _snap = k.snapshot(SnapshotKind::Full);

    // List snapshots
    let entries: Vec<_> = std::fs::read_dir("snapshots").unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "json"))
        .collect();
    for e in &entries {
        println!("  snapshot: {}", e.path().display());
    }

    let mut recovered = Kernel::recover_from_latest().unwrap();
    recovered.register_tools();

    let rx = recovered.eval("x").unwrap();
    let ry = recovered.eval("y").unwrap();
    assert_eq!(rx, Value::Int(100));
    assert_eq!(ry, Value::Int(200));
    println!("File recovery: x={}, y={}", rx, ry);
}

#[test]
fn test_model_invoke_works() {
    let mut k = make_kernel();

    // Basic Lisp test first
    let r = k.eval("(+ 1 2)");
    assert!(r.is_ok());
    assert_eq!(r.unwrap(), Value::Int(3));

    // Try the model/invoke native
    let r = k.eval(r#"(model/invoke "Reply with EXACTLY 'plh-e2e-ok' and nothing else.")"#);
    match &r {
        Ok(val) => {
            println!("model/invoke result: {}", val);
            match val {
                Value::Tagged { family, variant, fields } => {
                    if variant == "Ok" {
                        let text = &fields[0];
                        match text {
                            Value::String(s) => {
                                println!("Model response: {}", s);
                                assert!(s.contains("plh-e2e-ok"), "response should contain marker: got {}", s);
                            }
                            _ => println!("Text field not string: {:?}", text),
                        }
                    } else if variant == "Err" {
                        let err_text = &fields[0];
                        println!("Model returned error: {}", err_text);
                        // May be API key issue — not a hard failure
                        if err_text.to_string().contains("No API key") {
                            println!("WARNING: No API key configured");
                        } else {
                            panic!("Model error: {}", err_text);
                        }
                    }
                }
                _ => panic!("expected Tagged value"),
            }
        }
        Err(e) => {
            let err_str = e.to_string();
            println!("model/invoke error: {}", err_str);
            if err_str.contains("No API key") {
                println!("WARNING: No API key configured, model test skipped");
            } else {
                panic!("unexpected error: {}", err_str);
            }
        }
    }
}

#[test]
fn test_define_data_and_match() {
    let mut k = make_kernel();

    k.eval(r#"(define-data result/Result (Ok value) (Err problem))"#).unwrap();

    let binding = k.eval(r#"(user/result/Result/Ok "test")"#);
    assert!(binding.is_ok(), "constructor should work: {:?}", binding.err());

    let matched = k.eval(r#"
        (match (user/result/Result/Ok 42)
          ((result/Result/Ok n) n)
          ((result/Result/Err msg) -1))
    "#);
    assert!(matched.is_ok(), "match: {:?}", matched.err());
    assert_eq!(matched.unwrap(), Value::Int(42));
    println!("define-data + match: OK");
}

#[test]
fn test_agent_think() {
    let mut k = make_kernel();

    let r = k.eval(r#"(agent/think "Context: testing" "Say EXACTLY 'plh-think-test' and nothing else.")"#);
    match &r {
        Ok(val) => {
            println!("agent/think result: {}", val);
            match val {
                Value::Tagged { family, variant, fields } => {
                    if variant == "Ok" {
                        println!("Think response: {}", fields[0]);
                    } else {
                        println!("Think error: {}", fields[0]);
                    }
                }
                _ => println!("Think result: {}", val),
            }
        }
        Err(e) => println!("agent/think error: {}", e),
    }
}
