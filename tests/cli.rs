use std::process::Command;

fn huddle(args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_huddle")).args(args).env("SCRUM_DB", ":memory:").output().unwrap();
    (o.status.success(), String::from_utf8_lossy(if o.status.success() { &o.stdout } else { &o.stderr }).into_owned())
}

#[test]
fn the_binary_prints_json_and_errors_to_stderr() {
    assert_eq!(huddle(&["list"]), (true, "[]\n".into()));
    let (ok, out) = huddle(&["add", "Write docs", "--due", "2026-11-02"]);
    assert!(ok);
    let t: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!((t["id"].as_str(), t["status"].as_str(), t["due"].as_str()), (Some("#1"), Some("todo"), Some("Mon 2 Nov, 18:00")));
    let (ok, err) = huddle(&["move", "#1"]);
    assert!(!ok && err.starts_with("Error: missing status"));
    // no command and no terminal: print the usage instead of opening the board
    assert!(huddle(&[]).1.starts_with("huddle [<command>"));
}
