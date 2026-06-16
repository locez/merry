mod support;

use support::merry_without_openai_env;

#[test]
fn root_help_does_not_advertise_tui_subcommand() {
    let output = merry_without_openai_env()
        .arg("--help")
        .output()
        .expect("merry --help should run");

    assert!(output.status.success());
    let stdout = std::str::from_utf8(&output.stdout).expect("stdout should be utf-8");
    assert!(stdout.contains("Usage: merry [OPTIONS] [COMMAND]"));
    assert!(!stdout.contains("merry tui"));
}
