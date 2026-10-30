use std::path::Path;

use alacritty_terminal::{
    Term,
    event::VoidListener,
    grid::Dimensions,
    term::{Config, TermMode},
    vte::ansi::Processor,
};

#[derive(Clone, Copy)]
struct TestSize;

impl Dimensions for TestSize {
    fn total_lines(&self) -> usize {
        3
    }

    fn screen_lines(&self) -> usize {
        3
    }

    fn columns(&self) -> usize {
        20
    }
}

#[test]
fn pinned_alacritty_exposes_the_required_public_terminal_api() {
    let mut terminal = Term::new(Config::default(), &TestSize, VoidListener);
    let mut processor: Processor = Processor::new();
    processor.advance(&mut terminal, b"public-api");
    terminal.resize(TestSize);
    let _ = terminal.grid();
    let _ = terminal.mode().contains(TermMode::SHOW_CURSOR);
    let _ = terminal.selection_to_string();
    let _ = terminal.damage();
    terminal.reset_damage();
}

#[test]
fn terminal_runtime_dependency_and_module_contract_is_exact() {
    const FORK_URL: &str = "https://github.com/MoyuTeams/alacritty";
    const FORK_REV: &str = "78f36350907b9145a16a6f12e01bfec34ad5b3dd";

    fn quoted_value<'a>(fields: impl Iterator<Item = &'a str>, key: &str) -> &'a str {
        let values: Vec<_> = fields
            .filter_map(|field| field.split_once('='))
            .filter(|(name, _)| name.trim() == key)
            .map(|(_, value)| {
                value
                    .trim()
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .expect("依赖字段须为字符串")
            })
            .collect();
        assert_eq!(values.len(), 1, "{key} 必须恰好声明一次");
        values[0]
    }

    let session = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = session.parent().unwrap().parent().unwrap();
    let manifest = std::fs::read_to_string(session.join("Cargo.toml")).unwrap();
    let lock = std::fs::read_to_string(root.join("Cargo.lock"))
        .unwrap()
        .replace("\r\n", "\n");
    let modules = std::fs::read_to_string(session.join("src/lib.rs")).unwrap();

    // 检查实际声明而非注释；该依赖保留原版本约束，不混入 path/branch/tag。
    let declarations: Vec<_> = manifest
        .lines()
        .map(|line| line.split('#').next().unwrap().trim())
        .filter_map(|line| line.split_once('='))
        .filter(|(name, _)| name.trim() == "alacritty_terminal")
        .map(|(_, value)| value.trim())
        .collect();
    assert_eq!(declarations.len(), 1);
    let fields = declarations[0]
        .strip_prefix('{')
        .and_then(|value| value.strip_suffix('}'))
        .expect("alacritty_terminal 必须为精确团队 git 依赖");
    assert_eq!(fields.split(',').count(), 3);
    assert_eq!(quoted_value(fields.split(','), "git"), FORK_URL);
    assert_eq!(quoted_value(fields.split(','), "rev"), FORK_REV);
    assert_eq!(quoted_value(fields.split(','), "version"), "=0.26.0");

    let packages: Vec<_> = lock
        .split("[[package]]")
        .filter(|package| {
            package
                .lines()
                .any(|line| line.trim() == "name = \"alacritty_terminal\"")
        })
        .collect();
    assert_eq!(packages.len(), 1, "不得留下第二个 alacritty_terminal 实例");
    assert_eq!(quoted_value(packages[0].lines(), "version"), "0.26.0");
    assert_eq!(
        quoted_value(packages[0].lines(), "source"),
        format!("git+{FORK_URL}?rev={FORK_REV}#{FORK_REV}")
    );
    let session_package = lock
        .split("[[package]]")
        .find(|package| {
            package
                .lines()
                .any(|line| line.trim() == "name = \"rshell-session\"")
        })
        .unwrap();
    assert!(
        session_package
            .lines()
            .any(|line| line.trim() == "\"alacritty_terminal\",")
    );
    for forbidden in ["wezterm-term", "termwiz", "d69264df"] {
        assert!(!lock.contains(forbidden), "lockfile contains {forbidden}");
        assert!(
            !manifest.contains(forbidden),
            "manifest contains {forbidden}"
        );
    }
    for removed in ["wezterm_adapter", "wezterm_input", "wezterm_writer"] {
        assert!(!modules.contains(removed));
        assert!(!session.join(format!("src/{removed}.rs")).exists());
    }
}
