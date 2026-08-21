//! mcp — a minimal Model Context Protocol client over stdio.
//!
//! Scope: the stdio transport only. No HTTP, no SSE, no OAuth. Servers come
//! from the `[mcp]` table in `config.toml`, one `name = "command args…"`
//! line each. Each server is spawned once per process, handshaked
//! (initialize → notifications/initialized → tools/list), and every tool it
//! advertises is re-exported alongside slag's natives as
//! `mcp__<server>__<tool>`.
//!
//! Failure policy: a server that will not spawn, will not handshake, or
//! answers too slowly is dropped with a warning and the forge continues. An
//! MCP server is an optional extra, never a reason to crack a run.
//!
//! Shutdown rides the stdio contract rather than an explicit kill: the
//! registry lives in a `static` that never drops, so when slag exits the OS
//! closes its end of each stdin pipe and the server reads EOF. That is how
//! every stdio MCP server is expected to stop.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use super::ToolSpec;
use crate::error::SlagError;

/// Protocol revision slag announces at initialize. Slag negotiates no
/// optional capabilities, so a server answering with an older revision is
/// accepted as-is.
const PROTOCOL_VERSION: &str = "2025-06-18";
/// Spawn, initialize, and tools/list must all land inside this window.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Ceiling for one tools/call round trip.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Tool namespace. `dispatch` routes any call with this prefix here.
pub const PREFIX: &str = "mcp__";
const SEP: &str = "__";
/// A server advertising more than this many tools would swamp the prompt;
/// the rest are dropped with a note in the connect warning.
const MAX_TOOLS_PER_SERVER: usize = 40;
/// tools/call text output cap, matching the bash tool's default.
const OUTPUT_CAP: usize = 30_000;

// ---------------------------------------------------------------------------
// Wire protocol: newline-delimited JSON-RPC 2.0 over the child's stdio.
// ---------------------------------------------------------------------------

struct Conn {
    /// Held to keep the child alive and killed on drop; never polled.
    _child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl Conn {
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, SlagError> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        // Server-initiated notifications, log lines, and answers to earlier
        // requests all share this pipe: read until the matching id lands.
        loop {
            let line = self
                .stdout
                .next_line()
                .await
                .map_err(|e| SlagError::Tool(format!("read failed: {e}")))?
                .ok_or_else(|| SlagError::Tool("server closed its stdout".into()))?;
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue; // not JSON: a banner or stray print, not our answer
            };
            if msg.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                let detail = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unspecified error");
                return Err(SlagError::Tool(format!("{method}: {detail}")));
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn notify(&mut self, method: &str) -> Result<(), SlagError> {
        self.send(&json!({"jsonrpc": "2.0", "method": method}))
            .await
    }

    async fn send(&mut self, msg: &Value) -> Result<(), SlagError> {
        let mut line =
            serde_json::to_string(msg).map_err(|e| SlagError::Tool(format!("encode: {e}")))?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| SlagError::Tool(format!("write failed: {e}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| SlagError::Tool(format!("flush failed: {e}")))
    }
}

// ---------------------------------------------------------------------------
// Servers and the registry.
// ---------------------------------------------------------------------------

struct McpTool {
    spec: ToolSpec,
    /// The server's own name for this tool, before namespacing.
    original: String,
}

struct Server {
    name: String,
    tools: Vec<McpTool>,
    conn: Mutex<Conn>,
}

#[derive(Default)]
pub struct Registry {
    servers: Vec<Server>,
}

impl Registry {
    /// Spawn and handshake each `(name, command)` pair. Returns the registry
    /// plus one warning line per server that did not come up, so the caller
    /// decides how loudly to report them.
    pub async fn connect(configured: Vec<(String, String)>) -> (Self, Vec<String>) {
        let mut servers = Vec::new();
        let mut warnings = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (name, command) in configured {
            let name = sanitize(&name);
            if name.is_empty() || !seen.insert(name.clone()) {
                warnings.push(format!("mcp: skipped duplicate or unnamed server '{name}'"));
                continue;
            }
            match connect_one(&name, &command).await {
                Ok(server) => servers.push(server),
                Err(e) => {
                    warnings.push(format!("mcp: server '{name}' unavailable ({})", reason(&e)))
                }
            }
        }
        (Self { servers }, warnings)
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.servers
            .iter()
            .flat_map(|s| s.tools.iter().map(|t| t.spec.clone()))
            .collect()
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.servers
            .iter()
            .flat_map(|s| s.tools.iter().map(|t| t.spec.name.clone()))
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    pub fn server_names(&self) -> Vec<String> {
        self.servers.iter().map(|s| s.name.clone()).collect()
    }

    /// `(servers, tools)` for the startup line.
    pub fn counts(&self) -> (usize, usize) {
        (
            self.servers.len(),
            self.servers.iter().map(|s| s.tools.len()).sum(),
        )
    }

    /// Invoke a namespaced tool. One call per server at a time: the stdio
    /// pipe carries a single request/response stream, so the mutex is the
    /// correctness boundary, not an optimization.
    pub async fn call(&self, name: &str, args: &Value) -> Result<String, SlagError> {
        let (server, original) = self
            .servers
            .iter()
            .find_map(|s| {
                s.tools
                    .iter()
                    .find(|t| t.spec.name == name)
                    .map(|t| (s, t.original.as_str()))
            })
            .ok_or_else(|| SlagError::Tool(format!("unknown MCP tool: {name}")))?;
        let params = json!({"name": original, "arguments": args});
        let mut conn = server.conn.lock().await;
        let result = tokio::time::timeout(CALL_TIMEOUT, conn.request("tools/call", params))
            .await
            .map_err(|_| {
                SlagError::Tool(format!(
                    "{name} timed out after {}s",
                    CALL_TIMEOUT.as_secs()
                ))
            })??;
        render(&result)
    }
}

async fn connect_one(name: &str, command: &str) -> Result<Server, SlagError> {
    let argv = shell_words::split(command)
        .map_err(|e| SlagError::Tool(format!("cannot parse command: {e}")))?;
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| SlagError::Tool("empty command".into()))?;
    // stderr goes to the void: servers log there freely, and mixing it into
    // tool output would corrupt every result.
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| SlagError::Tool(format!("cannot spawn {program}: {e}")))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| SlagError::Tool("no stdin pipe".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SlagError::Tool("no stdout pipe".into()))?;
    let mut conn = Conn {
        _child: child,
        stdin,
        stdout: BufReader::new(stdout).lines(),
        next_id: 0,
    };

    let listed = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(&mut conn))
        .await
        .map_err(|_| {
            SlagError::Tool(format!(
                "handshake timed out after {}s",
                HANDSHAKE_TIMEOUT.as_secs()
            ))
        })??;

    let mut tools = Vec::new();
    let mut exposed: HashSet<String> = HashSet::new();
    for advertised in listed.iter().take(MAX_TOOLS_PER_SERVER) {
        let Some(tool) = to_tool(name, advertised) else {
            continue;
        };
        if exposed.insert(tool.spec.name.clone()) {
            tools.push(tool);
        }
    }
    if tools.is_empty() {
        return Err(SlagError::Tool("advertised no usable tools".into()));
    }
    Ok(Server {
        name: name.to_string(),
        tools,
        conn: Mutex::new(conn),
    })
}

/// initialize → notifications/initialized → tools/list, returning the raw
/// tool descriptors.
async fn handshake(conn: &mut Conn) -> Result<Vec<Value>, SlagError> {
    conn.request(
        "initialize",
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "slag", "version": env!("CARGO_PKG_VERSION")},
        }),
    )
    .await?;
    conn.notify("notifications/initialized").await?;
    let listed = conn.request("tools/list", json!({})).await?;
    Ok(listed
        .get("tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// One `tools/list` entry into a namespaced spec. A descriptor without a
/// usable name is dropped; a missing schema becomes an empty object, which
/// every provider accepts.
fn to_tool(server: &str, advertised: &Value) -> Option<McpTool> {
    let original = advertised.get("name")?.as_str()?.to_string();
    let safe = sanitize(&original);
    if safe.is_empty() {
        return None;
    }
    let description = advertised
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("(no description)");
    let parameters = advertised
        .get("inputSchema")
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    Some(McpTool {
        spec: ToolSpec {
            name: format!("{PREFIX}{server}{SEP}{safe}"),
            description: format!("[mcp:{server}] {description}"),
            parameters,
        },
        original,
    })
}

/// Flatten a tools/call result into text. Text blocks concatenate;
/// non-text blocks (images, audio, embedded resources) are named but not
/// inlined, since the smith's transcript is text. `isError: true` comes
/// back as a tool error so the model sees a failure, not a success with
/// sad content.
fn render(result: &Value) -> Result<String, SlagError> {
    let mut out = String::new();
    for block in result
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => out.push_str(block.get("text").and_then(Value::as_str).unwrap_or("")),
            Some(kind) => out.push_str(&format!("[{kind} content omitted]")),
            None => continue,
        }
        out.push('\n');
    }
    // Structured-only results (no content blocks) still carry the answer.
    if out.trim().is_empty() {
        if let Some(structured) = result.get("structuredContent") {
            out = serde_json::to_string_pretty(structured).unwrap_or_default();
        }
    }
    let out = super::tools::truncate_middle(out.trim_end(), OUTPUT_CAP);
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(SlagError::Tool(out));
    }
    Ok(out)
}

/// `SlagError::Tool` displays as "tool error: …", which misdescribes a
/// spawn or handshake failure. Startup warnings carry the bare reason.
fn reason(e: &SlagError) -> String {
    let text = e.to_string();
    text.strip_prefix("tool error: ")
        .unwrap_or(&text)
        .to_string()
}

/// Tool names travel to providers that accept `[A-Za-z0-9_-]` only.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Process-global registry. Servers outlive individual toolboxes: parallel
// anvils each build their own ToolBox, and every one of them shares these
// connections rather than spawning a private copy of every server.
// ---------------------------------------------------------------------------

static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// Connect every server in the `[mcp]` config table. Idempotent: a second
/// call after the registry is set is a no-op. Returns the warning lines for
/// the caller to print.
pub async fn connect_configured() -> Vec<String> {
    if REGISTRY.get().is_some() {
        return Vec::new();
    }
    let (registry, warnings) = Registry::connect(crate::config::mcp_servers()).await;
    let _ = REGISTRY.set(registry);
    warnings
}

pub fn registry() -> Option<&'static Registry> {
    REGISTRY.get()
}

/// Specs to advertise beside the natives. Empty until `connect_configured`
/// runs, which keeps every test and plan pass on the native eight.
pub fn specs() -> Vec<ToolSpec> {
    registry().map(Registry::specs).unwrap_or_default()
}

pub fn tool_names() -> Vec<String> {
    registry().map(Registry::tool_names).unwrap_or_default()
}

/// Does this tool name belong to an MCP server?
pub fn handles(name: &str) -> bool {
    name.starts_with(PREFIX)
}

pub async fn call(name: &str, args: &Value) -> Result<String, SlagError> {
    match registry() {
        Some(registry) => registry.call(name, args).await,
        None => Err(SlagError::Tool(format!(
            "{name}: no MCP servers are connected"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    /// A POSIX-sh MCP server: enough of the protocol to handshake, list one
    /// tool, and answer a call. Echoes each request's own id back, so it
    /// stays correct however slag numbers its requests.
    fn fake_server(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        write!(f, "#!/bin/sh\n{body}").unwrap();
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    const ECHO_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"protocolVersion\":\"2025-06-18\"}}" ;;
    *'"tools/list"'*)
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"tools\":[{\"name\":\"echo\",\"description\":\"Echo text back\",\"inputSchema\":{\"type\":\"object\",\"properties\":{\"text\":{\"type\":\"string\"}}}}]}}" ;;
    *'"tools/call"'*)
      echo '{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info"}}'
      echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"pong\"}]}}" ;;
  esac
done
"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn connects_lists_and_calls_a_stdio_server() {
        let dir = tempfile::tempdir().unwrap();
        let server = fake_server(dir.path(), "echo-server", ECHO_SERVER);
        let (registry, warnings) =
            Registry::connect(vec![("demo".into(), format!("sh {}", server.display()))]).await;
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(registry.counts(), (1, 1));

        let specs = registry.specs();
        assert_eq!(specs[0].name, "mcp__demo__echo");
        assert!(
            specs[0].description.contains("[mcp:demo]"),
            "{:?}",
            specs[0]
        );
        assert_eq!(specs[0].parameters["properties"]["text"]["type"], "string");

        // A notification arriving before the answer must not be mistaken
        // for it.
        let out = registry
            .call("mcp__demo__echo", &json!({"text": "ping"}))
            .await
            .expect("call succeeds");
        assert_eq!(out, "pong");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unknown_tool_and_dead_server_fail_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let server = fake_server(dir.path(), "echo-server", ECHO_SERVER);
        let (registry, warnings) = Registry::connect(vec![
            ("demo".into(), format!("sh {}", server.display())),
            ("ghost".into(), "slag-no-such-binary-42".into()),
        ])
        .await;
        assert_eq!(registry.counts(), (1, 1));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("ghost"), "{warnings:?}");

        let err = registry
            .call("mcp__demo__missing", &json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown MCP tool"), "{err}");
    }

    #[test]
    fn tool_names_are_namespaced_and_provider_safe() {
        let tool = to_tool(
            "my-server",
            &json!({"name": "read file!", "description": "d", "inputSchema": {"type": "object"}}),
        )
        .expect("named tool");
        assert_eq!(tool.spec.name, "mcp__my-server__read_file_");
        assert_eq!(tool.original, "read file!");
        assert!(handles(&tool.spec.name));
        // A nameless descriptor is dropped rather than exposed blank.
        assert!(to_tool("s", &json!({"description": "d"})).is_none());
    }

    #[test]
    fn missing_schema_becomes_an_empty_object() {
        let tool = to_tool("s", &json!({"name": "t"})).unwrap();
        assert_eq!(tool.spec.parameters["type"], "object");
        assert!(tool.spec.description.contains("(no description)"));
        // A non-object schema is replaced, not passed through.
        let tool = to_tool("s", &json!({"name": "t", "inputSchema": "nonsense"})).unwrap();
        assert_eq!(tool.spec.parameters["type"], "object");
    }

    #[test]
    fn render_flattens_text_and_surfaces_tool_errors() {
        let ok = render(&json!({"content": [
            {"type": "text", "text": "line one"},
            {"type": "image", "data": "…"},
            {"type": "text", "text": "line two"}
        ]}))
        .unwrap();
        assert_eq!(ok, "line one\n[image content omitted]\nline two");

        let structured = render(&json!({"structuredContent": {"rows": 2}})).unwrap();
        assert!(structured.contains("\"rows\": 2"), "{structured}");

        let err = render(&json!({
            "isError": true,
            "content": [{"type": "text", "text": "file not found"}]
        }))
        .unwrap_err();
        assert!(err.to_string().contains("file not found"), "{err}");
    }

    #[test]
    fn absent_registry_reports_instead_of_panicking() {
        // The global is never set in unit tests; the accessors stay empty
        // so plan passes and the native eight are unaffected.
        assert!(specs().is_empty());
        assert!(tool_names().is_empty());
        assert!(!handles("bash"));
    }
}
