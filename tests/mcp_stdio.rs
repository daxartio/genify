#![cfg(feature = "cli")]

use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value as JsonValue, json};

#[test]
fn mcp_can_plan_diff_and_apply_simple_config() {
    let root = temp_root("stdio");
    let config = json!({
        "props": {
            "name": "demo"
        },
        "rules": [
            {
                "type": "file",
                "path": "out.txt",
                "content": "Hello {{ name }}"
            }
        ]
    });

    let mut client = McpClient::start(&root);
    client.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "clientInfo": {
                "name": "genify-test",
                "version": "0.0.0"
            },
            "capabilities": {}
        }),
    );
    let init = client.response();
    assert_eq!(init["result"]["serverInfo"]["name"], "genify");
    client.notification("notifications/initialized", json!({}));

    client.request(2, "tools/list", json!({}));
    let tools = client.response();
    let tool_names = tools["result"]["tools"]
        .as_array()
        .expect("tools should be an array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool should have a name"))
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"genify_plan"));
    assert!(tool_names.contains(&"genify_apply"));

    client.request(
        3,
        "tools/call",
        json!({
            "name": "genify_plan",
            "arguments": {
                "config": config.clone()
            }
        }),
    );
    let plan = client.response();
    assert_eq!(
        plan["result"]["structuredContent"]["affected_paths"],
        json!(["out.txt"])
    );
    assert!(!root.join("out.txt").exists());

    client.request(
        4,
        "tools/call",
        json!({
            "name": "genify_diff",
            "arguments": {
                "config": config.clone()
            }
        }),
    );
    let diff = client.response();
    let diff_text = diff["result"]["structuredContent"]["diff"]
        .as_str()
        .expect("diff should be a string");
    assert!(diff_text.contains("+Hello demo"));
    assert!(!root.join("out.txt").exists());

    client.request(
        5,
        "tools/call",
        json!({
            "name": "genify_apply",
            "arguments": {
                "config": config,
                "explicit_approval": true
            }
        }),
    );
    let apply = client.response();
    assert_eq!(
        apply["result"]["structuredContent"]["changed_files"],
        json!(["out.txt"])
    );
    assert_eq!(
        fs::read_to_string(root.join("out.txt")).expect("output should exist"),
        "Hello demo\n"
    );

    client.shutdown();
    let _ = fs::remove_dir_all(root);
}

#[test]
fn mcp_validate_config_returns_json_hint() {
    let root = temp_root("validate-hint");
    let mut client = McpClient::start(&root);
    client.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "clientInfo": {
                "name": "genify-test",
                "version": "0.0.0"
            },
            "capabilities": {}
        }),
    );
    let _ = client.response();
    client.notification("notifications/initialized", json!({}));

    client.request(
        2,
        "tools/call",
        json!({
            "name": "genify_validate_config",
            "arguments": {
                "config": {}
            }
        }),
    );
    let response = client.response();
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["valid"], false);
    assert_eq!(content["diagnostics"][0]["code"], "missing_rules");
    assert_eq!(
        content["hint"]["minimal_config"]["rules"][0]["type"],
        "replace"
    );

    client.shutdown();
    let _ = fs::remove_dir_all(root);
}

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl McpClient {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_genify"))
            .args(["mcp", "--root"])
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("MCP server should start");
        let stdin = child.stdin.take().expect("stdin should be piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout should be piped"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn request(&mut self, id: u64, method: &str, params: JsonValue) {
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        self.write_message(request);
    }

    fn notification(&mut self, method: &str, params: JsonValue) {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        self.write_message(notification);
    }

    fn response(&mut self) -> JsonValue {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("response should be readable");
        assert!(!line.is_empty(), "server closed stdout");
        serde_json::from_str(&line).expect("response should be JSON")
    }

    fn write_message(&mut self, message: JsonValue) {
        writeln!(
            self.stdin,
            "{}",
            serde_json::to_string(&message).expect("message should serialize")
        )
        .expect("request should be written");
        self.stdin.flush().expect("request should be flushed");
    }

    fn shutdown(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

fn temp_root(name: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("genify-mcp-{name}-{suffix}"));
    fs::create_dir_all(&path).expect("temp root should be created");
    path
}
