//! The browser bridge: the Zyris extension in Chrome or Firefox, offered to an agent as the MCP
//! server `browser`.
//!
//! **Two processes that never meet any other way.** The browser starts `zyris` as its native
//! messaging host when the extension calls `connectNative`, and the node starts `zyris
//! browser-mcp` as one of its MCP servers. Neither parent knows the other exists, so the two
//! children meet on a local socket: the MCP side listens, the host side dials. The host is a pure
//! byte pipe — the socket carries native messaging's own framing (a native-endian `u32` length,
//! then JSON) — so every decision about what a message means is made here, in the MCP side.
//!
//! **Both roles are picked before `clap` sees the arguments** ([`role`]), for two reasons. A
//! browser starts a host with arguments of its own (`chrome-extension://…/`, or Firefox's manifest
//! path and extension ID), which `clap` would refuse; and `main`'s Windows `attach_console`
//! rebinds stdout to a console, which would cut both roles off from the pipe they talk over.
//!
//! [`link`] is the other half: what the Settings screen's "Link browsers" button writes so the two
//! processes get started at all.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context as _, bail};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ServerHandler, ServiceExt as _};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _, WriteHalf};
use tokio::sync::{Mutex, oneshot};

/// What the host is registered under, in both browsers' native messaging manifests.
pub const HOST_NAME: &str = "cc.attacca.zyris";
/// The extension's Firefox ID, pinned in its `browser_specific_settings`.
const FIREFOX_ID: &str = "zyris@attacca.cc";
/// The extension's Chrome ID for an unpacked build, pinned by the `key` in its manifest.
// shortcut: the Web Store assigns its own ID; add it here once the listing exists.
const CHROME_ORIGINS: &[&str] = &["chrome-extension://dombmgnljfikkmhnjohcajlmokngogpk/"];
/// The first argument `link` writes into the MCP server entry, and what [`role`] looks for.
const MCP_ARG: &str = "browser-mcp";
/// The MCP server entry's name; the capability an agent addresses is `mcp_browser`.
const SERVER_NAME: &str = "browser";
/// How long one call waits for the extension. A page that takes longer to load than this is
/// answered by the next `page_text`, not by holding the agent.
const CALL_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq)]
pub enum Role {
    /// Started by a browser: pipe its stdin and stdout to the socket.
    Host,
    /// Started by the node as an MCP server: listen on the socket.
    Mcp,
}

/// Which bridge role this run is, if any, from the raw arguments.
pub fn role(args: &[String]) -> Option<Role> {
    match args.get(1).map(String::as_str) {
        Some(MCP_ARG) => Some(Role::Mcp),
        Some(first) if first.starts_with("chrome-extension://") => Some(Role::Host),
        _ if args.iter().any(|arg| arg == FIREFOX_ID) => Some(Role::Host),
        _ => None,
    }
}

pub fn run(role: Role) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    match role {
        Role::Host => runtime.block_on(host()),
        Role::Mcp => runtime.block_on(mcp()),
    }
}

/// Where the two halves meet.
// shortcut: always the default instance's, so a `--server` run cannot be linked; give the MCP
// entry the instance name as an argument if development runs ever need a browser.
#[cfg(unix)]
fn socket_path() -> PathBuf {
    crate::instance::data_dir("zyris").join("browser.sock")
}

#[cfg(windows)]
fn pipe_name() -> String {
    // Pipe names are machine-wide, so the user is part of it.
    let user = std::env::var("USERNAME").unwrap_or_default();
    format!(r"\\.\pipe\{HOST_NAME}.browser.{user}")
}

async fn host() -> anyhow::Result<()> {
    #[cfg(unix)]
    let socket = tokio::net::UnixStream::connect(socket_path()).await;
    #[cfg(windows)]
    let socket = tokio::net::windows::named_pipe::ClientOptions::new().open(pipe_name());
    // Exiting is how the extension learns Zyris is not running; it tries again on its own.
    let socket = socket.context("Zyris is not running, or the browser link is not enabled")?;
    let (mut from_app, mut to_app) = tokio::io::split(socket);
    let (mut stdin, mut stdout) = (tokio::io::stdin(), tokio::io::stdout());
    write_frame(&mut stdout, &json!({ "hello": true })).await?;
    tokio::select! {
        result = tokio::io::copy(&mut stdin, &mut to_app) => result?,
        result = tokio::io::copy(&mut from_app, &mut stdout) => result?,
    };
    Ok(())
}

async fn mcp() -> anyhow::Result<()> {
    let bridge = Arc::new(Bridge::default());
    tokio::spawn(listen(bridge.clone()));
    let service = Server(bridge).serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(unix)]
async fn listen(bridge: Arc<Bridge>) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let path = socket_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    // Left behind by a node that did not exit cleanly; the instance lock means nobody else owns it.
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    loop {
        let (stream, _) = listener.accept().await?;
        bridge.attach(stream).await;
    }
}

#[cfg(windows)]
async fn listen(bridge: Arc<Bridge>) -> anyhow::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let name = pipe_name();
    let mut server = ServerOptions::new().first_pipe_instance(true).create(&name)?;
    loop {
        server.connect().await?;
        let connected = std::mem::replace(&mut server, ServerOptions::new().create(&name)?);
        bridge.attach(connected).await;
    }
}

type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// One connected browser: where to write requests, and who is waiting for which answer.
struct Browser {
    id: u64,
    writer: Mutex<Writer>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
}

/// Every browser connected right now. The newest one answers calls, so with Chrome and Firefox
/// both open the agent drives whichever was started last, and the other takes over if it closes.
#[derive(Default)]
struct Bridge {
    browsers: Mutex<Vec<Arc<Browser>>>,
    next: AtomicU64,
}

impl Bridge {
    async fn attach<S: AsyncRead + AsyncWrite + Send + 'static>(self: &Arc<Self>, stream: S) {
        let (mut reader, writer): (_, WriteHalf<S>) = tokio::io::split(stream);
        let browser = Arc::new(Browser {
            id: self.next.fetch_add(1, Ordering::Relaxed),
            writer: Mutex::new(Box::new(writer)),
            pending: Mutex::default(),
        });
        self.browsers.lock().await.push(browser.clone());
        let bridge = self.clone();
        tokio::spawn(async move {
            while let Ok(reply) = read_frame(&mut reader).await {
                let Some(id) = reply.get("id").and_then(Value::as_u64) else { continue };
                if let Some(waiter) = browser.pending.lock().await.remove(&id) {
                    let _ = waiter.send(reply);
                }
            }
            // Dropping the waiters answers every call still in flight with "disconnected". Cleared
            // by hand: each of those calls holds this `Browser`, so it is not dropped on its own.
            browser.pending.lock().await.clear();
            bridge.browsers.lock().await.retain(|other| other.id != browser.id);
        });
    }

    /// Ask the newest browser to run one method, and wait for its answer.
    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let browser = self.browsers.lock().await.last().cloned().ok_or(
            "No browser is connected. Install the Zyris extension, then press \"Link browsers\" \
             in Zyris's Settings.",
        )?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (send, answer) = oneshot::channel();
        browser.pending.lock().await.insert(id, send);
        let request = json!({ "id": id, "method": method, "params": params });
        write_frame(&mut *browser.writer.lock().await, &request)
            .await
            .map_err(|error| format!("could not reach the browser: {error}"))?;
        let reply = match tokio::time::timeout(CALL_DEADLINE, answer).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => return Err("the browser disconnected before answering".into()),
            Err(_) => {
                browser.pending.lock().await.remove(&id);
                return Err(format!("the browser did not answer within {CALL_DEADLINE:?}"));
            }
        };
        match reply.get("error").and_then(Value::as_str) {
            Some(error) => Err(error.to_string()),
            None => Ok(reply.get("result").cloned().unwrap_or(Value::Null)),
        }
    }
}

async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> std::io::Result<Value> {
    let mut length = [0; 4];
    reader.read_exact(&mut length).await?;
    let mut body = vec![0; u32::from_ne_bytes(length) as usize];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

async fn write_frame(writer: &mut (impl AsyncWrite + Unpin + ?Sized), value: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(value)?;
    writer.write_all(&(body.len() as u32).to_ne_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await
}

/// The tools, each one named after the extension method it calls.
fn tools() -> Vec<Tool> {
    let tab_id = json!({ "type": "integer", "description": "A tab id from tabs_list. Defaults to the active tab." });
    let url = json!({ "type": "string", "description": "The address to load." });
    let schema = |properties: Value, required: &[&str]| {
        let Value::Object(object) =
            json!({ "type": "object", "properties": properties, "required": required })
        else {
            unreachable!()
        };
        object
    };
    vec![
        Tool::new("tabs_list", "Every open tab: id, window, title, address, and whether it is active.", schema(json!({}), &[])),
        Tool::new("tab_open", "Open an address in a new tab.", schema(json!({ "url": url }), &["url"])),
        Tool::new("tab_navigate", "Load an address in an existing tab.", schema(json!({ "tabId": tab_id, "url": url }), &["url"])),
        Tool::new("page_text", "The visible text of a tab's page.", schema(json!({ "tabId": tab_id }), &[])),
        Tool::new("screenshot", "A PNG of what a tab shows on screen. Brings the tab to the front.", schema(json!({ "tabId": tab_id }), &[])),
    ]
}

struct Server(Arc<Bridge>);

impl ServerHandler for Server {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("zyris-browser", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let params = Value::Object(request.arguments.unwrap_or_default());
        // A failure the browser reported is the tool's answer, not a protocol error.
        let result = match self.0.call(&request.name, params).await {
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
            Ok(mut value) if request.name == "screenshot" => {
                let png = value["png"].take();
                CallToolResult::success(vec![ContentBlock::image(png.as_str().unwrap_or_default(), "image/png")])
            }
            Ok(value) => CallToolResult::success(vec![ContentBlock::text(value.to_string())]),
        };
        Ok(result.into())
    }
}

/// Whether this instance's server list already starts the bridge.
pub fn linked(data: &Path) -> bool {
    zyris_mcp::Config::read(data)
        .is_ok_and(|config| config.servers.iter().any(|server| is_ours(server) && server.enabled))
}

fn is_ours(server: &zyris_mcp::ServerConfig) -> bool {
    server.name == SERVER_NAME && server.args.first().map(String::as_str) == Some(MCP_ARG)
}

/// Register the host with every browser this machine has, and add the bridge to the server list.
///
/// Answers with the browsers it registered with. The server list is read once at startup, so the
/// caller restarts for the link to take effect.
pub fn link(data: &Path) -> anyhow::Result<Vec<&'static str>> {
    let home = directories::BaseDirs::new().context("no home directory")?.home_dir().to_path_buf();
    // An AppImage runs from a mount that is gone after it exits; the browser has to start the file.
    let exe = match std::env::var_os("APPIMAGE") {
        Some(appimage) => PathBuf::from(appimage),
        // shortcut: on Nix this is a store path, so linking again is needed after an upgrade.
        None => std::env::current_exe()?,
    };
    link_in(&home, data, &exe)
}

fn link_in(home: &Path, data: &Path, exe: &Path) -> anyhow::Result<Vec<&'static str>> {
    let chrome = json!({
        "name": HOST_NAME,
        "description": "Zyris",
        "path": exe,
        "type": "stdio",
        "allowed_origins": CHROME_ORIGINS,
    });
    let firefox = json!({
        "name": HOST_NAME,
        "description": "Zyris",
        "path": exe,
        "type": "stdio",
        "allowed_extensions": [FIREFOX_ID],
    });

    let mut registered = Vec::new();
    for (browser, root, hosts, manifest) in browsers(home) {
        // Only browsers that are there: a directory for one that is not installed is clutter.
        if !root.is_dir() {
            continue;
        }
        let manifest = if manifest == Kind::Firefox { &firefox } else { &chrome };
        std::fs::create_dir_all(&hosts)?;
        std::fs::write(hosts.join(format!("{HOST_NAME}.json")), serde_json::to_vec_pretty(manifest)?)?;
        registered.push(browser);
    }
    #[cfg(windows)]
    registered.extend(register_windows(data, &chrome, &firefox)?);
    if registered.is_empty() {
        bail!("found no Chrome, Chromium, Brave, Edge or Firefox for this user");
    }

    let mut config = zyris_mcp::Config::read(data)?;
    if let Some(other) = config.servers.iter().find(|server| server.name == SERVER_NAME && !is_ours(server)) {
        bail!("the server list already has a server called `{}` that is not the browser link", other.name);
    }
    config.servers.retain(|server| !is_ours(server));
    config.servers.push(zyris_mcp::ServerConfig {
        name: SERVER_NAME.into(),
        command: exe.display().to_string(),
        args: vec![MCP_ARG.into()],
        enabled: true,
    });
    std::fs::create_dir_all(data)?;
    let mut bytes = serde_json::to_vec_pretty(&config)?;
    bytes.push(b'\n');
    std::fs::write(zyris_mcp::Config::path(data), bytes)?;
    Ok(registered)
}

#[derive(PartialEq)]
enum Kind {
    Chromium,
    Firefox,
}

/// Each browser: its name, the directory that says it is installed, where its hosts go, and
/// which manifest it reads. Windows registers through the registry instead; see `register_windows`.
fn browsers(home: &Path) -> Vec<(&'static str, PathBuf, PathBuf, Kind)> {
    let mut list = Vec::new();
    let mut add = |name, root: PathBuf, hosts: &str, kind| {
        let hosts = root.join(hosts);
        list.push((name, root, hosts, kind));
    };
    if cfg!(target_os = "linux") {
        let config = home.join(".config");
        add("Chrome", config.join("google-chrome"), "NativeMessagingHosts", Kind::Chromium);
        add("Chromium", config.join("chromium"), "NativeMessagingHosts", Kind::Chromium);
        add("Brave", config.join("BraveSoftware/Brave-Browser"), "NativeMessagingHosts", Kind::Chromium);
        add("Edge", config.join("microsoft-edge"), "NativeMessagingHosts", Kind::Chromium);
        add("Firefox", home.join(".mozilla"), "native-messaging-hosts", Kind::Firefox);
    } else if cfg!(target_os = "macos") {
        let support = home.join("Library/Application Support");
        add("Chrome", support.join("Google/Chrome"), "NativeMessagingHosts", Kind::Chromium);
        add("Chromium", support.join("Chromium"), "NativeMessagingHosts", Kind::Chromium);
        add("Brave", support.join("BraveSoftware/Brave-Browser"), "NativeMessagingHosts", Kind::Chromium);
        add("Edge", support.join("Microsoft Edge"), "NativeMessagingHosts", Kind::Chromium);
        add("Firefox", support.join("Mozilla"), "NativeMessagingHosts", Kind::Firefox);
    }
    list
}

/// On Windows a browser finds its hosts through `HKCU`, pointing at a manifest anywhere on disk.
/// Chrome's key is read by Chromium, Brave and Edge as well.
#[cfg(windows)]
fn register_windows(data: &Path, chrome: &Value, firefox: &Value) -> anyhow::Result<Vec<&'static str>> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut registered = Vec::new();
    for (browser, key, manifest) in [
        ("Chrome", r"Software\Google\Chrome", chrome),
        ("Firefox", r"Software\Mozilla", firefox),
    ] {
        let path = data.join(format!("{HOST_NAME}.{browser}.json"));
        std::fs::create_dir_all(data)?;
        std::fs::write(&path, serde_json::to_vec_pretty(manifest)?)?;
        let key = format!(r"HKCU\{key}\NativeMessagingHosts\{HOST_NAME}");
        let status = std::process::Command::new("reg")
            .args(["add", &key, "/ve", "/t", "REG_SZ", "/d"])
            .arg(&path)
            .arg("/f")
            .creation_flags(CREATE_NO_WINDOW)
            .status()?;
        if !status.success() {
            bail!("`reg add {key}` failed with {status}");
        }
        registered.push(browser);
    }
    Ok(registered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn each_browser_starts_a_host_and_the_node_starts_the_server() {
        assert_eq!(role(&args(&["zyris", "chrome-extension://abc/"])), Some(Role::Host));
        assert_eq!(role(&args(&["zyris", "chrome-extension://abc/", "--parent-window=7"])), Some(Role::Host));
        assert_eq!(role(&args(&["zyris", "/home/a/.mozilla/x.json", FIREFOX_ID])), Some(Role::Host));
        assert_eq!(role(&args(&["zyris", MCP_ARG])), Some(Role::Mcp));
        assert_eq!(role(&args(&["zyris"])), None);
        assert_eq!(role(&args(&["zyris", "status"])), None);
    }

    #[tokio::test]
    async fn a_call_reaches_the_newest_browser_and_comes_back_with_its_answer() {
        let bridge = Arc::new(Bridge::default());
        assert!(bridge.call("tabs_list", json!({})).await.unwrap_err().contains("No browser"));

        let (ours, mut extension) = tokio::io::duplex(1 << 16);
        bridge.attach(ours).await;
        let answering = tokio::spawn(async move {
            let request = read_frame(&mut extension).await.unwrap();
            assert_eq!(request["method"], "page_text");
            let id = request["id"].clone();
            write_frame(&mut extension, &json!({ "id": id, "result": { "text": "hi" } })).await.unwrap();
            let request = read_frame(&mut extension).await.unwrap();
            write_frame(&mut extension, &json!({ "id": request["id"], "error": "no active tab" })).await.unwrap();
        });

        assert_eq!(bridge.call("page_text", json!({})).await.unwrap(), json!({ "text": "hi" }));
        assert_eq!(bridge.call("screenshot", json!({})).await.unwrap_err(), "no active tab");
        answering.await.unwrap();
    }

    #[tokio::test]
    async fn a_browser_that_goes_away_fails_its_call_instead_of_hanging_it() {
        let bridge = Arc::new(Bridge::default());
        let (ours, extension) = tokio::io::duplex(1 << 16);
        bridge.attach(ours).await;
        let call = tokio::spawn({
            let bridge = bridge.clone();
            async move { bridge.call("tabs_list", json!({})).await }
        });
        tokio::task::yield_now().await;
        drop(extension);
        assert!(call.await.unwrap().unwrap_err().contains("disconnected"));
    }

    #[test]
    fn linking_registers_installed_browsers_only_and_adds_one_server() {
        let home = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let exe = Path::new("/opt/zyris/zyris");
        if cfg!(windows) {
            return;
        }
        assert!(link_in(home.path(), data.path(), exe).is_err(), "no browser, nothing to link");

        let firefox = if cfg!(target_os = "macos") { "Library/Application Support/Mozilla" } else { ".mozilla" };
        std::fs::create_dir_all(home.path().join(firefox)).unwrap();
        assert_eq!(link_in(home.path(), data.path(), exe).unwrap(), vec!["Firefox"]);
        // Twice is the same as once.
        link_in(home.path(), data.path(), exe).unwrap();

        let hosts = if cfg!(target_os = "macos") { "NativeMessagingHosts" } else { "native-messaging-hosts" };
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(home.path().join(firefox).join(hosts).join(format!("{HOST_NAME}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["allowed_extensions"], json!([FIREFOX_ID]));
        assert_eq!(manifest["path"], "/opt/zyris/zyris");

        let config = zyris_mcp::Config::read(data.path()).unwrap();
        assert_eq!(config.servers.len(), 1);
        assert!(linked(data.path()));
    }
}
