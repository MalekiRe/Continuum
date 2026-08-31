use persistent_lisp_harness::{Kernel, Value};

fn temp_dir() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("continuum-snapshots-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

fn rewrite_snapshot(
    directory: &std::path::Path,
    id: &persistent_lisp_harness::SnapshotId,
    mutate: impl FnOnce(&mut serde_json::Value),
) {
    use sha2::{Digest, Sha256};
    let path = std::fs::read_dir(directory)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.to_string_lossy().contains(id.as_str()))
        .unwrap();
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    mutate(&mut envelope["kernel"]);
    envelope["checksum"] = hex::encode(Sha256::digest(
        serde_json::to_vec(&envelope["kernel"]).unwrap(),
    ))
    .into();
    std::fs::write(path, serde_json::to_vec(&envelope).unwrap()).unwrap();
}

#[test]
fn recovery_chooses_newest_snapshot() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel.eval_value("(define answer 1)").unwrap();
    kernel.snapshot().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    kernel.eval_value("(define answer 2)").unwrap();
    kernel.append_transcript("answer", "2");
    kernel.snapshot().unwrap();

    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.eval_value("answer").unwrap(), Value::Int(2));
    assert_eq!(recovered.frames()[0].state().transcript()[0].result, "2");
    assert_eq!(
        recovered.eval_value("(+ 1 2)").unwrap(),
        Value::Int(3),
        "natives were not restored"
    );
    assert_eq!(recovered.snapshot_count(), 2);
}

#[test]
fn corrupt_newest_snapshot_falls_back_to_previous_valid_one() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel.eval_value("(define answer 1)").unwrap();
    kernel.snapshot().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    kernel.eval_value("(define answer 2)").unwrap();
    let newest = kernel.snapshot().unwrap();
    let newest_path = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.extension().is_some_and(|e| e == "json")
                && p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(newest.id.as_str())
        })
        .unwrap();
    std::fs::write(&newest_path, b"{truncated").unwrap();
    std::fs::write(
        dir.join("inc-snap-99999999.json"),
        br#"{"not_kernel":true}"#,
    )
    .unwrap();

    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.eval_value("answer").unwrap(), Value::Int(1));
}

#[test]
fn checksum_tampering_is_detected() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel.eval_value("(define answer 7)").unwrap();
    kernel.snapshot().unwrap();
    let path = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .unwrap();
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    json["kernel"]["next_frame_id"] = serde_json::Value::from(999_999);
    std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
    assert!(
        Kernel::recover_from_dir(&dir)
            .unwrap_err()
            .to_string()
            .contains("checksum")
    );
}

#[test]
fn closure_heap_restores_across_real_snapshot() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel
        .eval_value("(define (make-adder n) (lambda (x) (+ x n)))")
        .unwrap();
    kernel.eval_value("(define add5 (make-adder 5))").unwrap();
    kernel.snapshot().unwrap();
    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.eval_value("(add5 3)").unwrap(), Value::Int(8));
}

#[test]
fn child_stack_transcripts_and_selected_memory_survive_snapshot() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel.append_transcript("(+ 1 1)", "2");
    let child = kernel
        .spawn_subagent("researcher", "inspect state")
        .unwrap();
    kernel
        .eval_value(r#"(memory/remember "finding" "snapshot-safe")"#)
        .unwrap();
    kernel.append_transcript("(source/list)", "()");
    kernel.snapshot().unwrap();

    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.frames().len(), 2);
    assert_eq!(
        recovered.frames()[0].state().transcript()[0].source,
        "(+ 1 1)"
    );
    assert_eq!(recovered.frames()[1].id(), &child);
    assert!(
        recovered.frames()[1]
            .state()
            .instructions()
            .contains("inspect state")
    );
    assert_eq!(
        recovered.frames()[1].state().transcript()[0].source,
        "(source/list)"
    );
    assert_eq!(recovered.frames()[1].state().memory()[0].key, "finding");
    assert_eq!(
        recovered.frames()[1].state().memory()[0].value,
        "snapshot-safe"
    );
}

#[test]
fn native_registration_does_not_pollute_binding_history() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel.snapshot().unwrap();
    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert!(recovered.environment().binding_history_len("kernel", "+") == 0);
}

#[test]
fn pending_human_message_id_survives_snapshot() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    let id = kernel.human_message("persist this question").unwrap();
    kernel.snapshot().unwrap();
    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert!(recovered.has_pending_message(&id));
    let root = recovered.frames()[0].id().clone();
    assert!(recovered.notices_for_frame(&root).iter().any(|notice| {
        notice.id.as_ref().map(|id| id.as_str()) == Some(id.as_str())
            && notice.text == "persist this question"
    }));
}

#[test]
fn snapshot_collects_unreachable_closure_environments() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    for _ in 0..100 {
        kernel.eval_value("(let ((x 1)) (lambda () x))").unwrap();
    }
    assert!(kernel.lexical_arena_counts().0 >= 100);
    kernel.snapshot().unwrap();
    assert_eq!(kernel.lexical_arena_counts().0, 1);
    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.lexical_arena_counts().0, 1);
}

#[test]
fn mutated_closure_cells_survive_snapshot_recovery() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel
        .eval_value("(define counter (let ((x 0)) (lambda () (set! x (+ x 1)) x)))")
        .unwrap();
    assert_eq!(kernel.eval_value("(counter)").unwrap(), Value::Int(1));
    kernel.snapshot().unwrap();

    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.eval_value("(counter)").unwrap(), Value::Int(2));
    assert_eq!(recovered.eval_value("(counter)").unwrap(), Value::Int(3));
}

#[test]
fn first_class_native_alias_survives_snapshot_recovery() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(&dir);
    kernel.eval_value("(define add +)").unwrap();
    assert_eq!(kernel.eval_value("(add 2 3)").unwrap(), Value::Int(5));
    kernel.snapshot().unwrap();

    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.eval_value("(add 20 22)").unwrap(), Value::Int(42));
}

#[test]
fn missing_snapshot_directory_is_typed_and_not_created() {
    let dir = std::env::temp_dir().join(format!("continuum-missing-{}", uuid::Uuid::new_v4()));
    assert!(matches!(
        Kernel::recover_from_dir(&dir),
        Err(persistent_lisp_harness::SnapshotError::NotFound)
    ));
    assert!(!dir.exists());
}

#[test]
fn recovered_snapshot_rebinds_its_storage_directory() {
    let original = temp_dir();
    let relocated = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(&original);
    kernel.snapshot().unwrap();
    let source = std::fs::read_dir(&original)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::copy(&source, relocated.join(source.file_name().unwrap())).unwrap();

    let mut recovered = Kernel::recover_from_dir(&relocated).unwrap();
    recovered.snapshot().unwrap();
    assert_eq!(std::fs::read_dir(&original).unwrap().count(), 1);
    assert_eq!(std::fs::read_dir(&relocated).unwrap().count(), 2);
}

#[test]
fn recovery_rejects_a_dangling_lexical_cursor() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(&dir);
    let snapshot = kernel.snapshot().unwrap();
    rewrite_snapshot(&dir, &snapshot.id, |kernel| {
        kernel["env"]["current_environment"] = serde_json::json!(999);
    });
    assert!(matches!(
        Kernel::recover_from_dir(&dir),
        Err(persistent_lisp_harness::SnapshotError::AllInvalid(_))
    ));
}

#[test]
fn recovery_rejects_invalid_notice_targets_and_cursors() {
    for invalid_cursor in [false, true] {
        let dir = temp_dir();
        let mut kernel = Kernel::new();
        kernel.set_snapshot_directory(&dir);
        kernel.human_message("pending").unwrap();
        let snapshot = kernel.snapshot().unwrap();
        rewrite_snapshot(&dir, &snapshot.id, |kernel| {
            if invalid_cursor {
                kernel["frames"][0]["notice_cursor"] = serde_json::json!(u64::MAX);
            } else {
                kernel["notices"][0]["target_frames"] = serde_json::json!([]);
            }
        });
        assert!(matches!(
            Kernel::recover_from_dir(&dir),
            Err(persistent_lisp_harness::SnapshotError::AllInvalid(_))
        ));
    }
}

#[test]
fn recovery_rejects_a_closure_with_a_missing_captured_environment() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(&dir);
    kernel.eval_value("(define f (lambda (x) x))").unwrap();
    let snapshot = kernel.snapshot().unwrap();
    rewrite_snapshot(&dir, &snapshot.id, |kernel| {
        kernel["env"]["namespaces"]["user"]["bindings"]["f"]["Function"]["env_id"] =
            serde_json::json!(999);
    });
    assert!(matches!(
        Kernel::recover_from_dir(&dir),
        Err(persistent_lisp_harness::SnapshotError::AllInvalid(_))
    ));
}

#[test]
fn recovered_allocator_exhaustion_is_handled_without_overflow() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(&dir);
    let snapshot = kernel.snapshot().unwrap();
    rewrite_snapshot(&dir, &snapshot.id, |kernel| {
        kernel["next_notice_sequence"] = serde_json::json!(u64::MAX - 1);
        kernel["next_frame_id"] = serde_json::json!(u64::MAX - 1);
    });

    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    recovered.human_message("still allocates safely").unwrap();
    recovered.spawn_subagent("last", "task").unwrap();
    assert!(matches!(
        recovered.spawn_subagent("exhausted", "task"),
        Err(persistent_lisp_harness::AllocationError::Exhausted("frame"))
    ));
}

#[test]
fn snapshots_round_trip_maps_with_arbitrary_lisp_keys() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(&dir);
    kernel
        .eval_value("(define user/mixed-map {:mode \"fast\" 1 \"one\" '(a b) 3})")
        .unwrap();
    kernel.snapshot().unwrap();

    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(
        recovered.eval_value("(map/get mixed-map :mode)").unwrap(),
        Value::string("fast")
    );
    assert_eq!(
        recovered.eval_value("(map/get mixed-map 1)").unwrap(),
        Value::string("one")
    );
    assert_eq!(
        recovered.eval_value("(map/get mixed-map '(a b))").unwrap(),
        Value::Int(3)
    );
}

#[test]
fn returned_external_traps_are_not_part_of_snapshots() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(&dir);
    let outcome = kernel.eval(r#"(bash "printf hi")"#).unwrap();
    assert!(matches!(
        outcome,
        persistent_lisp_harness::EvalOutcome::Trap(persistent_lisp_harness::TrapRequest {
            operation: persistent_lisp_harness::VmTrap::RunBash { .. },
            ..
        })
    ));
    kernel.snapshot().unwrap();
    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.eval_value("(+ 20 22)").unwrap(), Value::Int(42));
}

#[test]
fn recovery_preserves_the_serial_agent_stack() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(&dir);
    kernel.spawn_subagent("worker", "task").unwrap();
    kernel.human_message("redirect parent later").unwrap();
    kernel.snapshot().unwrap();
    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.frames().len(), 2);
    assert_eq!(recovered.frames()[0].name(), "root");
    assert_eq!(recovered.frames()[1].name(), "worker");
}

#[test]
fn binding_history_does_not_retain_obsolete_closure_heaps() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(&dir);
    kernel
        .eval_value("(define retained (let ((large 42)) (lambda () large)))")
        .unwrap();
    assert!(kernel.lexical_arena_counts().0 > 1);
    kernel.eval_value("(define (retained) 0)").unwrap();
    kernel.snapshot().unwrap();
    assert_eq!(kernel.lexical_arena_counts().0, 1);
}

#[test]
fn recovery_rejects_a_different_runtime_fingerprint() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(&dir);
    kernel.snapshot().unwrap();
    let path = std::fs::read_dir(&dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    envelope["runtime_fingerprint"] = "different-runtime".into();
    std::fs::write(path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    assert!(matches!(
        Kernel::recover_from_dir(&dir),
        Err(persistent_lisp_harness::SnapshotError::AllInvalid(_))
    ));
}
