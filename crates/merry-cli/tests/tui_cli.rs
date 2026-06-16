mod support;

use support::merry_without_openai_env;

#[test]
fn root_without_subcommand_routes_to_tui_stub() {
    let output = merry_without_openai_env()
        .output()
        .expect("merry should run");

    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "stub failure should not write stdout"
    );
    let stderr = std::str::from_utf8(&output.stderr).expect("stderr should be utf-8");
    assert!(stderr.contains("TUI is not implemented yet"));
    assert!(!stderr.contains("Usage: merry"));
}

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
