//! The MCP server driven as an MCP client drives it: JSON-RPC messages in,
//! answers out, on an engine with no window.

use std::path::PathBuf;

use iai_engine::{Engine, Policy, Source};
use serde_json::{json, Value};

use crate::gate::{Gate, Roots};
use crate::host::LocalHost;
use crate::mcp::Server;

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("iai-mcp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Self(d)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn host() -> LocalHost {
    LocalHost::new(Engine::new(
        iai_engine::cmds::core_registry(),
        Source::Agent,
        Policy::agent(),
    ))
}

/// Call `method`, the answer's `result` (or the whole answer on an error).
fn rpc(server: &mut Server<'_>, method: &str, params: Value) -> Value {
    let msg = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let answer = server.handle_text(&msg.to_string()).expect("an answer");
    answer.get("result").cloned().unwrap_or(answer)
}

/// Call tool `name`; its structured result, or panic with the error.
fn tool(server: &mut Server<'_>, name: &str, args: Value) -> Value {
    let r = rpc(
        server,
        "tools/call",
        json!({"name": name, "arguments": args}),
    );
    assert_eq!(r["isError"], json!(false), "{name}: {r}");
    r["structuredContent"].clone()
}

fn tool_error(server: &mut Server<'_>, name: &str, args: Value) -> Value {
    let r = rpc(
        server,
        "tools/call",
        json!({"name": name, "arguments": args}),
    );
    assert_eq!(r["isError"], json!(true), "{name}: {r}");
    r["structuredContent"]["error"].clone()
}

#[test]
fn a_client_connects_and_lists_the_tools_it_may_use() {
    let mut host = host();
    let mut server = Server::new(
        &mut host,
        Gate::new(Policy::agent(), Roots::default()),
        "test",
    )
    .unwrap();
    let init = rpc(
        &mut server,
        "initialize",
        json!({"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
    );
    assert_eq!(init["protocolVersion"], "2025-03-26");
    assert_eq!(init["serverInfo"]["name"], "iai");
    assert!(init["instructions"]
        .as_str()
        .unwrap()
        .contains("analyze.region"));
    let unknown = rpc(
        &mut server,
        "initialize",
        json!({"protocolVersion": "1999-01-01"}),
    );
    assert_eq!(unknown["protocolVersion"], "2025-06-18");
    // A notification gets no answer.
    let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert!(server.handle_text(&note.to_string()).is_none());

    let tools = rpc(&mut server, "tools/list", json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for wanted in [
        "iai_doc_inspect",
        "iai_image_crop",
        "iai_ai_remove_background",
        "iai_analyze_region",
        "iai_command_list",
        "iai_command_run",
        "iai_command_batch",
        "iai_job_status",
        "iai_render_preview",
    ] {
        assert!(names.contains(&wanted), "{wanted} in {names:?}");
    }
    // No folder granted: no file tool; no network: no download.
    for absent in ["iai_file_open", "iai_export_pdf", "iai_ai_model_download"] {
        assert!(!names.contains(&absent), "{absent}");
    }
    let crop = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "iai_image_crop")
        .unwrap();
    assert_eq!(crop["inputSchema"]["type"], "object");
    assert!(!crop["inputSchema"].to_string().contains("$ref"));

    let missing = rpc(
        &mut server,
        "tools/call",
        json!({"name": "iai_nope", "arguments": {}}),
    );
    assert_eq!(missing["error"]["code"], crate::jsonrpc::INVALID_PARAMS);
    let no_method = rpc(&mut server, "sampling/createMessage", json!({}));
    assert_eq!(no_method["error"]["code"], crate::jsonrpc::METHOD_NOT_FOUND);
}

#[test]
fn an_agent_makes_a_page_checks_it_by_numbers_and_writes_a_pdf() {
    let (input, output) = (Dir::new("in"), Dir::new("out"));
    let roots = Roots::new(&[input.0.clone()], &[output.0.clone()]).unwrap();
    let mut host = host();
    let mut server = Server::new(&mut host, Gate::new(Policy::agent(), roots), "test").unwrap();

    let made = tool(
        &mut server,
        "iai_command_run",
        json!({"command": "file.new", "args": {"widthPx": 200, "heightPx": 100, "dpi": 300}}),
    );
    let doc = made["result"]["doc"].clone();
    tool(
        &mut server,
        "iai_vector_add_shape",
        json!({"shape": "rectangle", "rect": {"x": 0, "y": 0, "w": 100, "h": 100}, "fill": "#0090FF"}),
    );
    let measured = tool(
        &mut server,
        "iai_analyze_region",
        json!({"doc": doc, "rect": {"x": 10, "y": 10, "w": 50, "h": 50}}),
    );
    assert_eq!(measured["result"]["mean"], "#0090FF");
    assert!(measured["result"]["stdDev"].as_f64().unwrap() < 1.0);

    let pdf = tool(&mut server, "iai_export_pdf", json!({"path": "page.pdf"}));
    assert_eq!(pdf["result"]["pages"], json!(1));
    assert!(std::fs::read(output.0.join("page.pdf"))
        .unwrap()
        .starts_with(b"%PDF"));

    // Outside the folders: refused, nothing written.
    let e = tool_error(
        &mut server,
        "iai_export_pdf",
        json!({"path": "../escape.pdf"}),
    );
    assert_eq!(e["code"], "permission_denied");
    let e = tool_error(
        &mut server,
        "iai_command_run",
        json!({"command": "file.save", "args": {}}),
    );
    assert_eq!(e["code"], "bad_params");

    // A picture to look at.
    let r = rpc(
        &mut server,
        "tools/call",
        json!({"name": "iai_render_preview", "arguments": {"maxEdge": 64}}),
    );
    assert_eq!(r["content"][0]["type"], "image");
    assert_eq!(r["content"][0]["mimeType"], "image/png");
    assert!(r["content"][0]["data"].as_str().unwrap().len() > 100);

    // The session as a resource.
    let res = rpc(
        &mut server,
        "resources/read",
        json!({"uri": "iai://session"}),
    );
    let text = res["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"widthPx\": 200"), "{text}");
    let res = rpc(
        &mut server,
        "resources/read",
        json!({"uri": format!("iai://doc/{doc}")}),
    );
    assert!(res["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("layers"));
}

#[test]
fn an_atomic_batch_and_a_job_behave_over_mcp() {
    let mut host = host();
    let mut server = Server::new(
        &mut host,
        Gate::new(Policy::agent(), Roots::default()),
        "test",
    )
    .unwrap();
    tool(
        &mut server,
        "iai_command_run",
        json!({"command": "file.new", "args": {"widthPx": 40, "heightPx": 40}}),
    );
    let batch = tool(
        &mut server,
        "iai_command_batch",
        json!({"calls": [
            {"command": "layer.new", "args": {}},
            {"command": "layer.delete", "args": {"layer": 999}}
        ]}),
    );
    assert_eq!(batch["rolledBack"], json!(true));
    let info = tool(&mut server, "iai_doc_inspect", json!({}));
    assert_eq!(info["result"]["layers"].as_array().unwrap().len(), 1);

    // A command list filtered by prefix, with schemas.
    let list = tool(&mut server, "iai_command_list", json!({"prefix": "layer."}));
    let ids: Vec<&str> = list["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["layer.delete", "layer.move", "layer.new", "layer.set"]
    );
    assert!(list["commands"][0]["paramsSchema"]["properties"]["layer"].is_object());

    // Unknown job.
    let e = tool_error(&mut server, "iai_job_status", json!({"job": 77}));
    assert_eq!(e["code"], "not_found");
}

#[test]
fn the_server_serves_a_stream_line_by_line() {
    let mut host = host();
    let mut server = Server::new(
        &mut host,
        Gate::new(Policy::agent(), Roots::default()),
        "test",
    )
    .unwrap();
    let input = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}).to_string(),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        "not json".to_string(),
        json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}).to_string(),
    ]
    .join("\n");
    let mut out = Vec::new();
    server.serve(std::io::Cursor::new(input), &mut out).unwrap();
    let answers: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(answers.len(), 3);
    assert_eq!(answers[0]["id"], 1);
    assert_eq!(answers[1]["error"]["code"], crate::jsonrpc::PARSE_ERROR);
    assert_eq!(answers[2]["id"], 2);
}
