//! Drives the `tree-sitter-playground` binary over HTTP.

use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};

use serde_json::{Value, json};
use tree_sitter_playground::SUPPORTED_LANGUAGES;

struct Playground {
    process: Child,
    address: String,
}

impl Playground {
    fn start() -> Self {
        Self::start_on("127.0.0.1")
    }

    fn start_on(host: &str) -> Self {
        let mut process = Command::new(env!("CARGO_BIN_EXE_tree-sitter-playground"))
            .args(["--host", host, "--port", "0"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("playground should start");
        let stdout = process.stdout.take().expect("stdout is piped");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("playground should print its address");
        let address = line
            .trim()
            .strip_prefix("Playground running at http://")
            .unwrap_or_else(|| panic!("unexpected startup line: {line}"))
            .to_string();
        Self { process, address }
    }

    /// Sends one HTTP/1.1 request and returns the status code and body.
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> (u16, String) {
        let body = body.map(|body| body.to_string()).unwrap_or_default();
        let mut stream =
            TcpStream::connect(&self.address).expect("playground should accept connections");
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n{body}",
            self.address,
            body.len()
        )
        .expect("request should be sent");
        // A server that rejects a request before reading all of it can reset the connection after responding.
        let mut response = Vec::new();
        if let Err(error) = stream.read_to_end(&mut response) {
            assert!(
                error.kind() == ErrorKind::ConnectionReset && !response.is_empty(),
                "response should be read: {error}"
            );
        }
        let response = String::from_utf8(response).expect("response is UTF-8");
        let (head, body) = response
            .split_once("\r\n\r\n")
            .expect("response has headers");
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|status| status.parse().ok())
            .expect("response has a status code");
        (status, body.to_string())
    }

    fn post_ast(&self, language: &str, code: &str) -> (u16, Value) {
        let (status, body) = self.request(
            "POST",
            "/api/ast",
            Some(json!({ "language": language, "code": code })),
        );
        (
            status,
            serde_json::from_str(&body).expect("response is JSON"),
        )
    }
}

impl Drop for Playground {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[test]
fn test_serves_editor_page() {
    let playground = Playground::start();
    let (status, body) = playground.request("GET", "/", None);
    assert_eq!(status, 200);
    assert!(body.contains("<title>Tree-sitter Playground</title>"));
}

#[test]
fn test_binds_to_host_names() {
    let playground = Playground::start_on("localhost");
    let (status, _) = playground.request("GET", "/api/languages", None);
    assert_eq!(status, 200);
}

#[test]
fn test_lists_supported_languages() {
    let playground = Playground::start();
    let (status, body) = playground.request("GET", "/api/languages", None);
    assert_eq!(status, 200);
    let languages: Vec<String> = serde_json::from_str(&body).expect("languages are JSON");
    assert_eq!(languages, SUPPORTED_LANGUAGES);
}

#[test]
fn test_returns_ast_text_and_nodes_with_utf16_offsets() {
    let playground = Playground::start();
    let (status, response) = playground.post_ast("py", "s = \"한글\"\n");
    assert_eq!(status, 200);
    assert_eq!(response["language"], "python");
    assert_eq!(response["errorCount"], 0);

    let ast = response["ast"].as_str().expect("ast is text");
    let nodes = response["nodes"].as_array().expect("nodes is an array");
    assert_eq!(nodes.len(), ast.lines().count());

    let string = nodes
        .iter()
        .find(|node| node["label"] == "string")
        .expect("AST has a string node");
    assert_eq!(string["field"], "right");
    assert_eq!(string["start"], json!([0, 4]));
    assert_eq!(string["end"], json!([0, 12]));
    assert_eq!(string["from"], 4);
    assert_eq!(string["to"], 8);
}

#[test]
fn test_auto_detects_language() {
    let playground = Playground::start();
    let (status, response) = playground.post_ast("auto", "{\"name\": \"tree-sitter-playground\"}");
    assert_eq!(status, 200);
    assert_eq!(response["language"], "json");
}

#[test]
fn test_reports_syntax_errors() {
    let playground = Playground::start();
    let (status, response) = playground.post_ast("python", "def f():\n    x = = 1\n");
    assert_eq!(status, 200);
    assert!(response["errorCount"].as_u64() > Some(0));
    let nodes = response["nodes"].as_array().expect("nodes is an array");
    assert!(
        nodes
            .iter()
            .any(|node| node["isError"] == true && node["label"] == "ERROR")
    );
    assert_eq!(nodes[0]["label"], "module");
}

#[test]
fn test_rejects_unsupported_language() {
    let playground = Playground::start();
    let (status, response) = playground.post_ast("cobol", "DISPLAY 'HI'.");
    assert_eq!(status, 400);
    let error = response["error"].as_str().expect("error is text");
    assert!(error.contains("unsupported language `cobol`"), "{error}");
}

#[test]
fn test_reports_malformed_requests_as_json() {
    let playground = Playground::start();
    let (status, body) = playground.request("POST", "/api/ast", Some(json!({ "code": "x" })));
    assert_eq!(status, 422);
    let response: Value = serde_json::from_str(&body).expect("response is JSON");
    let error = response["error"].as_str().expect("error is text");
    assert!(error.contains("missing field `language`"), "{error}");
}

#[test]
fn test_rejects_requests_larger_than_the_limit() {
    let playground = Playground::start();
    // Just over the 256 KiB limit, so the server reads the whole request before rejecting it.
    let code = "x".repeat(256 * 1024);
    let (status, response) = playground.post_ast("python", &code);
    assert_eq!(status, 413);
    let error = response["error"].as_str().expect("error is text");
    assert!(error.contains("larger than 256 KiB"), "{error}");
}

#[test]
fn test_rejects_asts_larger_than_the_limit() {
    let playground = Playground::start();
    // Each nested array is indented one level deeper, so the AST would be about 100 MB.
    let code = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
    let (status, response) = playground.post_ast("json", &code);
    assert_eq!(status, 422);
    let error = response["error"].as_str().expect("error is text");
    assert!(error.contains("the AST is larger than"), "{error}");
}

#[cfg(unix)]
#[test]
fn test_shuts_down_gracefully_on_sigterm() {
    let mut playground = Playground::start();
    let pid = playground.process.id().to_string();
    let killed = Command::new("kill")
        .args(["-TERM", &pid])
        .status()
        .expect("kill should run");
    assert!(killed.success());
    for _ in 0..50 {
        if let Some(status) = playground.process.try_wait().expect("process status") {
            assert!(status.success(), "playground exited with {status}");
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("playground did not exit within 5 seconds of SIGTERM");
}
