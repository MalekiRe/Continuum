use persistent_lisp_harness::{Kernel, Value};

fn temp_dir() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("continuum-snapshots-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn recovery_chooses_newest_snapshot() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel.eval("(define answer 1)").unwrap();
    kernel.snapshot().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    kernel.eval("(define answer 2)").unwrap();
    kernel.append_transcript("answer", "2");
    kernel.snapshot().unwrap();

    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.eval("answer").unwrap(), Value::Int(2));
    assert_eq!(recovered.frames()[0].state().transcript()[0].result, "2");
    assert_eq!(
        recovered.eval("(+ 1 2)").unwrap(),
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
    kernel.eval("(define answer 1)").unwrap();
    kernel.snapshot().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    kernel.eval("(define answer 2)").unwrap();
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
    assert_eq!(recovered.eval("answer").unwrap(), Value::Int(1));
}

#[test]
fn checksum_tampering_is_detected() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel.eval("(define answer 7)").unwrap();
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
fn snapshot_refuses_pending_external_traps() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    let definition = "(define (twice x) (* x 2))";
    kernel.eval(definition).unwrap();
    kernel.append_transcript(definition, "twice");
    kernel.snapshot().unwrap();

    kernel.eval(r#"(bash "printf hi")"#).unwrap();
    assert!(kernel.has_trap());
    assert!(matches!(
        kernel.snapshot(),
        Err(persistent_lisp_harness::kernel::SnapshotError::Busy)
    ));

    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(
        recovered.environment().source("user/twice"),
        Some(definition)
    );
    assert!(!recovered.has_trap());
    let root = recovered.frames()[0].id().clone();
    assert!(recovered.notices_for_frame(&root).iter().any(|notice| {
        notice
            .text
            .contains("in-flight external operation was interrupted")
    }));
}

#[test]
fn closure_heap_restores_across_real_snapshot() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel
        .eval("(define (make-adder n) (lambda (x) (+ x n)))")
        .unwrap();
    kernel.eval("(define add5 (make-adder 5))").unwrap();
    kernel.snapshot().unwrap();
    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.eval("(add5 3)").unwrap(), Value::Int(8));
}

#[test]
fn child_stack_transcripts_and_selected_memory_survive_snapshot() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel.append_transcript("(+ 1 1)", "2");
    let child = kernel.spawn_subagent("researcher", "inspect state");
    kernel
        .eval(r#"(memory/remember "finding" "snapshot-safe")"#)
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
        kernel.eval("(let ((x 1)) (lambda () x))").unwrap();
    }
    assert!(kernel.lexical_arena_counts().0 >= 100);
    kernel.snapshot().unwrap();
    assert_eq!(kernel.lexical_arena_counts().0, 1);
    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.lexical_arena_counts().0, 1);
}

#[test]
fn v2_legacy_human_message_is_migrated_with_its_id() {
    use sha2::{Digest, Sha256};
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel.snapshot().unwrap();
    let path = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    envelope["format_version"] = 2.into();
    envelope["kind"] = "Full".into();
    envelope["kernel"]["frames"][0]["messages"] = serde_json::json!([]);
    envelope["kernel"]["frames"][0]["pending_message"] =
        "Human message [msg-legacy]: still here".into();
    envelope["kernel"]["frames"][0]["message_queue"] =
        serde_json::json!(["(system/HumanMessage \"duplicate\")"]);
    let payload = serde_json::to_vec(&envelope["kernel"]).unwrap();
    envelope["checksum"] = hex::encode(Sha256::digest(payload)).into();
    std::fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();

    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert!(recovered.has_pending_message(&persistent_lisp_harness::MessageId::new("msg-legacy")));
    assert_eq!(
        recovered
            .notices_for_frame(recovered.frames()[0].id())
            .into_iter()
            .find(|notice| notice.id.as_ref().map(|id| id.as_str()) == Some("msg-legacy"))
            .unwrap()
            .text,
        "still here"
    );
}

#[test]
fn mutated_closure_cells_survive_snapshot_recovery() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel
        .eval("(define counter (let ((x 0)) (lambda () (set! x (+ x 1)) x)))")
        .unwrap();
    assert_eq!(kernel.eval("(counter)").unwrap(), Value::Int(1));
    kernel.snapshot().unwrap();

    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.eval("(counter)").unwrap(), Value::Int(2));
    assert_eq!(recovered.eval("(counter)").unwrap(), Value::Int(3));
}

#[test]
fn recovery_discards_legacy_pending_bash_without_executing_it() {
    use sha2::{Digest, Sha256};
    let dir = temp_dir();
    let marker = dir.join("must-not-exist");
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel.snapshot().unwrap();
    let path = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    envelope["kernel"]["frames"][0]["state"]["pending_trap"] = serde_json::json!({
        "source": format!("(bash \"touch {}\")", marker.display()),
        "operation": { "RunBash": { "command": format!("touch {}", marker.display()) } }
    });
    envelope["checksum"] = hex::encode(Sha256::digest(
        serde_json::to_vec(&envelope["kernel"]).unwrap(),
    ))
    .into();
    std::fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();

    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert!(!recovered.has_trap());
    assert!(!marker.exists());
}

#[test]
fn genuine_v2_cloned_closure_heap_migrates_to_the_cell_arena() {
    use sha2::{Digest, Sha256};
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.set_snapshot_directory(dir.to_string_lossy().into_owned());
    kernel
        .eval("(define captured (let ((x 41)) (lambda () (+ x 1))))")
        .unwrap();
    kernel.snapshot().unwrap();
    let path = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .unwrap();
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let function =
        &envelope["kernel"]["env"]["namespaces"]["user"]["bindings"]["captured"]["Function"];
    let environment = function["env_id"].as_u64().unwrap();
    let environment_key = environment.to_string();
    let cell =
        envelope["kernel"]["env"]["lexical"]["environments"][&environment_key]["bindings"]["x"]
            .as_u64()
            .unwrap()
            .to_string();
    let value = envelope["kernel"]["env"]["lexical"]["cells"][&cell].clone();
    envelope["format_version"] = 2.into();
    envelope["kernel"]["lexical_heap"] = serde_json::json!({
        environment_key: [{ "x": value }]
    });
    envelope["kernel"]["env"]
        .as_object_mut()
        .unwrap()
        .remove("lexical");
    envelope["checksum"] = hex::encode(Sha256::digest(
        serde_json::to_vec(&envelope["kernel"]).unwrap(),
    ))
    .into();
    std::fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();

    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.eval("(captured)").unwrap(), Value::Int(42));
}
