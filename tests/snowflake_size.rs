use std::path::{Path, PathBuf};

fn code_lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .filter(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with("//")
        })
        .count()
}

#[test]
fn snowflake_stays_inside_explicit_loc_budgets() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/snowflake");
    let compiler = code_lines(&root.join("compile.rs"));
    let runtime: usize = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .filter(|path| path.file_name().is_some_and(|name| name != "compile.rs"))
        .map(|path| code_lines(&path))
        .sum();

    assert!(compiler <= 800, "snowflake compiler is {compiler} LOC");
    assert!(runtime <= 2_000, "snowflake runtime is {runtime} LOC");
    eprintln!("snowflake scaffold: compiler={compiler}, runtime={runtime}");
}
