use persistent_lisp_harness::{Kernel, Value, VmTrap};

fn temp_dir() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("continuum-snapshots-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn recovery_chooses_newest_snapshot() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.storage.snapshot_dir = dir.to_string_lossy().into_owned();
    kernel.eval("(define answer 1)").unwrap();
    kernel.snapshot().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    kernel.eval("(define answer 2)").unwrap();
    kernel.append_transcript("answer", "2");
    kernel.snapshot().unwrap();

    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.eval("answer").unwrap(), Value::Int(2));
    assert_eq!(recovered.frames[0].state.transcript[0].result, "2");
    assert_eq!(
        recovered.eval("(+ 1 2)").unwrap(),
        Value::Int(3),
        "natives were not restored"
    );
    assert_eq!(recovered.storage.snapshot_count, 2);
}

#[test]
fn corrupt_newest_snapshot_falls_back_to_previous_valid_one() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.storage.snapshot_dir = dir.to_string_lossy().into_owned();
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
                    .contains(&newest.id)
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
    kernel.storage.snapshot_dir = dir.to_string_lossy().into_owned();
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
            .contains("checksum")
    );
}

#[test]
fn pending_trap_source_and_transcript_survive_snapshot() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.storage.snapshot_dir = dir.to_string_lossy().into_owned();
    let definition = "(define (twice x) (* x 2))";
    kernel.eval(definition).unwrap();
    kernel.append_transcript(definition, "twice");
    kernel.eval(r#"(bash "printf hi")"#).unwrap();
    assert!(kernel.has_trap());
    kernel.snapshot().unwrap();

    let mut recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.env.source("user/twice"), Some(definition));
    assert_eq!(recovered.frames[0].state.transcript.len(), 1);
    assert!(
        matches!(recovered.take_trap(), Some(pending) if matches!(pending.operation, VmTrap::RunBash { ref command } if command == "printf hi"))
    );
}

#[test]
fn closure_heap_restores_across_real_snapshot() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.storage.snapshot_dir = dir.to_string_lossy().into_owned();
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
    kernel.storage.snapshot_dir = dir.to_string_lossy().into_owned();
    let parent = kernel.frames[0].id.clone();
    kernel.append_transcript_to(&parent, "(+ 1 1)", "2");
    let child = kernel.spawn_subagent("researcher", "inspect state");
    kernel
        .eval(r#"(memory/remember "finding" "snapshot-safe")"#)
        .unwrap();
    kernel.append_transcript_to(&child, "(source/list)", "()");
    kernel.snapshot().unwrap();

    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert_eq!(recovered.frames.len(), 2);
    assert_eq!(recovered.frames[0].state.transcript[0].source, "(+ 1 1)");
    assert_eq!(recovered.frames[1].id, child);
    assert!(
        recovered.frames[1]
            .state
            .instructions
            .contains("inspect state")
    );
    assert_eq!(
        recovered.frames[1].state.transcript[0].source,
        "(source/list)"
    );
    assert_eq!(recovered.frames[1].state.memory[0].key, "finding");
    assert_eq!(recovered.frames[1].state.memory[0].value, "snapshot-safe");
}

#[test]
fn native_registration_does_not_pollute_binding_history() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.storage.snapshot_dir = dir.to_string_lossy().into_owned();
    kernel.snapshot().unwrap();
    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert!(recovered.env.namespaces["kernel"].history("+").is_empty());
}

#[test]
fn pending_human_message_id_survives_snapshot() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.storage.snapshot_dir = dir.to_string_lossy().into_owned();
    let id = kernel.human_message("persist this question").unwrap();
    kernel.snapshot().unwrap();
    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert!(recovered.has_pending_message(&id));
    assert!(recovered.frames[0].messages.iter().any(|message| {
        message.id.as_deref() == Some(id.as_str()) && message.text == "persist this question"
    }));
}

#[test]
fn snapshot_collects_unreachable_closure_environments() {
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.storage.snapshot_dir = dir.to_string_lossy().into_owned();
    for _ in 0..100 {
        kernel.eval("(lambda (x) x)").unwrap();
    }
    assert!(kernel.lexical_heap.len() >= 100);
    kernel.snapshot().unwrap();
    assert!(kernel.lexical_heap.is_empty());
    let recovered = Kernel::recover_from_dir(&dir).unwrap();
    assert!(recovered.lexical_heap.is_empty());
}

#[test]
fn v2_legacy_human_message_is_migrated_with_its_id() {
    use sha2::{Digest, Sha256};
    let dir = temp_dir();
    let mut kernel = Kernel::new();
    kernel.storage.snapshot_dir = dir.to_string_lossy().into_owned();
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
    assert!(recovered.has_pending_message("msg-legacy"));
    assert_eq!(
        recovered.frames[0]
            .messages
            .iter()
            .find(|message| message.id.as_deref() == Some("msg-legacy"))
            .unwrap()
            .text,
        "still here"
    );
}
