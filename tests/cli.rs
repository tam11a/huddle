use std::io::Write;
use std::process::{Command, Stdio};

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

#[test]
fn mcp_speaks_json_rpc_over_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_huddle")).arg("mcp").env("SCRUM_DB", ":memory:")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for line in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"add_task","arguments":{"title":"Write docs"}}}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_tasks","arguments":{}}}"#,
    ] {
        writeln!(stdin, "{line}").unwrap();
    }
    drop(stdin); // EOF ends the server
    let out = String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap();
    let replies: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(replies.len(), 3); // the notification gets no reply
    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "huddle");
    let listed: serde_json::Value = serde_json::from_str(replies[2]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(listed[0]["title"], "Write docs");
}
