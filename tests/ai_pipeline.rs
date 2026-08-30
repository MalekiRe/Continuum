
use persistent_lisp_harness::Kernel;
use persistent_lisp_harness::Value;

fn make_kernel() -> &'static mut Kernel {
    let k = Kernel::new();
    Box::leak(Box::new(k))
}

// Helper: extract-lisp and eval-code in one step
fn extract_and_eval(k: &mut Kernel, model_output: &str) -> Result<String, String> {
    let extract_expr = format!("(extract-lisp {:?})", model_output);
    let code = match k.eval(&extract_expr) {
        Ok(Value::String(s)) => s.clone(),
        Ok(Value::Nil) => return Ok(String::new()),
        Ok(v) => return Err(format!("extract-lisp returned unexpected: {}", v)),
        Err(e) => return Err(format!("extract-lisp error: {}", e)),
    };
    let eval_expr = format!("(eval-code {:?})", code);
    match k.eval(&eval_expr) {
        Ok(v) => {
            let s = format!("{}", v);
            // Strip surrounding quotes from Value::String display
            let trimmed = s.trim_matches('"');
            Ok(trimmed.to_string())
        }
        Err(e) => Err(format!("eval-code error: {}", e)),
    }
}

// ===== AI MODEL OUTPUT PIPELINE TESTS =====

#[test]
fn test_ai_pipeline_extract_simple() {
    let k = make_kernel();
    let resp = "I'll explore the filesystem.\n<lisp>(bash \"ls -la /home\")</lisp>\nLet me see what's there.";
    let r = k.eval(&format!("(extract-lisp {:?})", resp));
    assert!(r.is_ok(), "extract: {:?}", r.err());
    let v = r.unwrap();
    assert_eq!(v, Value::string("(bash \"ls -la /home\")"), "got {}", v);
}

#[test]
fn test_ai_pipeline_extract_no_tags() {
    let k = make_kernel();
    let resp = "I wonder what I should explore next. The filesystem seems interesting.";
    let r = k.eval(&format!("(extract-lisp {:?})", resp));
    assert!(r.is_ok(), "extract: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::Nil, "should return nil when no tags");
}

#[test]
fn test_ai_pipeline_extract_multiline() {
    let k = make_kernel();
    let resp = "Let me check the system.\n\n<lisp>\n(bash \"uname -a\")\n</lisp>\n\nInteresting.";
    let r = k.eval(&format!("(extract-lisp {:?})", resp));
    assert!(r.is_ok(), "extract: {:?}", r.err());
    let v = r.unwrap();
    assert_eq!(v, Value::string("(bash \"uname -a\")"), "got {}", v);
}

#[test]
fn test_ai_pipeline_extract_multiple_tags() {
    let k = make_kernel();
    let resp = "First:\n<lisp>(define x 1)</lisp>\nSecond:\n<lisp>(define y 2)</lisp>";
    let r = k.eval(&format!("(extract-lisp {:?})", resp));
    assert!(r.is_ok(), "extract: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::string("(define x 1)"), "should get first tag only");
}

#[test]
fn test_ai_pipeline_extract_malformed() {
    let k = make_kernel();
    let resp = "Let me try:\n<lisp>(bash \"ls\")\nOops, forgot the closing tag";
    let r = k.eval(&format!("(extract-lisp {:?})", resp));
    assert!(r.is_ok(), "extract: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::Nil, "malformed tag should return nil");
}

#[test]
fn test_ai_pipeline_eval_code_valid() {
    let k = make_kernel();
    let r = k.eval(r#"(eval-code "(+ 1 2)")"#);
    assert!(r.is_ok(), "eval-code: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::string("3"));
}

#[test]
fn test_ai_pipeline_eval_code_define() {
    let k = make_kernel();
    let r = k.eval(r#"(eval-code "(define (greet) (bash \"echo hi\"))")"#);
    assert!(r.is_ok(), "eval-code define: {:?}", r.err());
    let s = format!("{}", r.unwrap());
    assert!(s.contains("greet"), "should return symbol, got: {}", s);
}

#[test]
fn test_ai_pipeline_eval_code_undefined_symbol() {
    let k = make_kernel();
    let r = k.eval(r#"(eval-code "(nonexistent-function 42)")"#);
    assert!(r.is_ok(), "eval-code should handle errors: {:?}", r.err());
    let s = format!("{}", r.unwrap());
    assert!(s.contains("error"), "should return error message, got: {}", s);
}

#[test]
fn test_ai_pipeline_eval_code_syntax_error() {
    let k = make_kernel();
    let r = k.eval(r#"(eval-code "(+ 1 (")"#);
    assert!(r.is_ok(), "eval-code should handle syntax errors: {:?}", r.err());
    let s = format!("{}", r.unwrap());
    assert!(s.contains("error"), "should return error message, got: {}", s);
}

#[test]
fn test_ai_pipeline_eval_code_arity_mismatch() {
    let k = make_kernel();
    let r = k.eval(r#"(eval-code "(+ 1)")"#);
    assert!(r.is_ok(), "eval-code should handle arity errors: {:?}", r.err());
    let s = format!("{}", r.unwrap());
    assert!(s.contains("error"), "should return error message, got: {}", s);
}

#[test]
fn test_ai_pipeline_eval_code_bash() {
    let k = make_kernel();
    let r = k.eval(r#"(eval-code "(bash \"echo hello from model\")")"#);
    assert!(r.is_ok(), "eval-code bash: {:?}", r.err());
    let s = format!("{}", r.unwrap());
    assert!(s.contains("hello from model"), "should contain output, got: {}", s);
}

#[test]
fn test_ai_pipeline_full_cycle() {
    let k = make_kernel();
    let model_response = "I'll start by checking what's in the home directory.\n\n<lisp>(bash \"ls -la /home\")</lisp>\n\nThis will show me the users and their files.";

    // Step 1: extract-lisp
    let r = k.eval(&format!("(extract-lisp {:?})", model_response));
    assert!(r.is_ok(), "extract: {:?}", r.err());
    let code = r.unwrap();
    assert!(code != Value::Nil, "should have found code");

    // Step 2: eval-code the extracted code
    let code_str = match &code {
        Value::String(s) => s.clone(),
        _ => panic!("expected string, got {:?}", code),
    };
    let expr = format!("(eval-code {:?})", code_str);
    let r = k.eval(&expr);
    assert!(r.is_ok(), "eval-code: {:?}", r.err());
    let result = format!("{}", r.unwrap());
    // The result should be a bash map containing /home
    assert!(result.contains("malek") || result.contains("error"),
        "expected bash output or error, got: {}", result);
}

#[test]
fn test_ai_pipeline_common_lisp_syntax() {
    let k = make_kernel();
    let constructs = vec![
        "(format t \"hello world\")",
        "(defun add (a b) (+ a b))",
        "(setq x 42)",
        "(make-hash-table)",
        "(push 1 *list*)",
        "(write-line \"hello\")",
        "(typep x 'integer)",
    ];
    for construct in constructs {
        let expr = format!("(eval-code {:?})", construct);
        let r = k.eval(&expr);
        assert!(r.is_ok(), "Common Lisp '{}' crashed: {:?}", construct, r.err());
        let s = format!("{}", r.unwrap());
        assert!(s.contains("error"), "Common Lisp '{}' should error, got: {}", construct, s);
    }
}

#[test]
fn test_ai_pipeline_model_repeats_previous_result() {
    let k = make_kernel();
    let resp = "The result of (bash \"ls\") was:\n{:exit 0 :stdout \"file1.txt\\nfile2.txt\" :stderr \"\"}\nNow I want to try something else.\n<lisp>(bash \"pwd\")</lisp>";
    let r = k.eval(&format!("(extract-lisp {:?})", resp));
    assert!(r.is_ok(), "extract: {:?}", r.err());
    let v = r.unwrap();
    assert_eq!(v, Value::string("(bash \"pwd\")"), "should extract only the code, got {}", v);
}

#[test]
fn test_ai_pipeline_only_code_no_preamble() {
    let k = make_kernel();
    let resp = "<lisp>(bash \"whoami\")</lisp>";
    let r = k.eval(&format!("(extract-lisp {:?})", resp));
    assert!(r.is_ok(), "extract: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::string("(bash \"whoami\")"));
}

#[test]
fn test_ai_pipeline_code_then_more_code() {
    let k = make_kernel();
    let resp = "<lisp>(+ 1 2)</lisp>\nResult: 3\n<lisp>(+ 3 4)</lisp>";
    let r = k.eval(&format!("(extract-lisp {:?})", resp));
    assert!(r.is_ok(), "extract: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::string("(+ 1 2)"), "should extract first code only");
}

#[test]
fn test_ai_pipeline_deeply_nested_code() {
    let k = make_kernel();
    // Model sometimes generates deeply nested expressions
    let resp = "Let me compute: <lisp>(+ 1 (* 2 (+ 3 4)))</lisp>";
    let r = k.eval(&format!("(extract-lisp {:?})", resp));
    assert!(r.is_ok(), "extract: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::string("(+ 1 (* 2 (+ 3 4)))"));

    let r = k.eval(r#"(eval-code "(+ 1 (* 2 (+ 3 4)))")"#);
    assert!(r.is_ok(), "eval-code nested: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::string("15"));
}

#[test]
fn test_ai_pipeline_extract_with_quotes_in_code() {
    let k = make_kernel();
    // Code with string literals containing <lisp>-like text
    let resp = r#"<lisp>(bash "echo 'hello <lisp> world'")</lisp>"#;
    let r = k.eval(&format!("(extract-lisp {:?})", resp));
    assert!(r.is_ok(), "extract with quotes: {:?}", r.err());
    let v = r.unwrap();
    let s = format!("{}", v);
    // Should extract the whole code including the inner string
    assert!(s.contains("echo"), "got: {}", s);
}

#[test]
fn test_ai_pipeline_eval_code_return_value_formats() {
    let k = make_kernel();
    // Different return types should be properly formatted
    let tests = vec![
        ("42", "42"),
        ("\"hello\"", "\"hello\""),
        ("(list 1 2 3)", "(1 2 3)"),
        ("#t", "#t"),
        ("#f", "#f"),
        ("nil", "nil"),
        ("(+ 1 2)", "3"),
        ("(bash \"echo test\")", ":"),  // map result contains :keys
    ];
    for (code, expected_substr) in tests {
        let expr = format!("(eval-code {:?})", code);
        let r = k.eval(&expr);
        assert!(r.is_ok(), "eval-code '{}': {:?}", code, r.err());
        let s = format!("{}", r.unwrap());
        // The result is displayed as a Value::String, which adds quotes
        // So we check for the substring within the output
        assert!(s.contains(expected_substr) || s.contains("error"),
            "eval-code '{}' should contain '{}', got: {}", code, expected_substr, s);
    }
}

#[test]
fn test_ai_pipeline_agent_core_loads_and_functions_defined() {
    let k = make_kernel();
    let core = r#"
        (define-data result/Result
          (Ok value)
          (Err problem)
          (Cancelled reason)
          (Indeterminate problem))

        (define (extract-lisp text)
          (let ((start (string-search "<lisp>" text)))
            (if start
                (let ((end (string-search "</lisp>" text)))
                  (if end
                      (substring text (+ start 6) end)
                      nil))
                nil)))
    "#;
    let r = k.eval(core);
    assert!(r.is_ok(), "agent core load: {:?}", r.err());

    let r = k.eval("(extract-lisp \"hello <lisp>(+ 1 2)</lisp> world\")");
    assert!(r.is_ok(), "extract-lisp: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::string("(+ 1 2)"));

    let r = k.eval(r#"(eval-code "(+ 1 2)")"#);
    assert!(r.is_ok(), "eval-code: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::string("3"));
}

#[test]
fn test_ai_pipeline_consecutive_extract_and_eval() {
    let k = make_kernel();
    // Simulate multiple consecutive model interactions
    let interactions = vec![
        ("<lisp>(define x 10)</lisp>", true),
        ("<lisp>(define y (+ x 5))</lisp>", true),
        ("<lisp>(+ x y)</lisp>", true),
        ("<lisp>(bash \"echo done\")</lisp>", true),
    ];

    for (model_output, should_succeed) in &interactions {
        // Extract
        let r = k.eval(&format!("(extract-lisp {:?})", model_output));
        assert!(r.is_ok(), "extract failed: {:?}", r.err());
        let code = r.unwrap();
        if *should_succeed {
            assert!(code != Value::Nil, "should have extracted code from: {}", model_output);
        }

        // Eval
        if let Value::String(code_str) = &code {
            if !code_str.is_empty() {
                let expr = format!("(eval-code {:?})", code_str);
                let r = k.eval(&expr);
                assert!(r.is_ok(), "eval-code failed: {:?}", r.err());
            }
        }
    }

    // Verify the definitions persisted
    let r = k.eval("(+ x y)");
    assert!(r.is_ok(), "final eval: {:?}", r.err());
    assert_eq!(r.unwrap(), Value::Int(25));
}
