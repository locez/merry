//! Session-scoped headless Chrome/Chromium lifecycle management for browser tools.

use super::{cdp::CdpSession, ws::connect_tcp_ws};
use serde::Deserialize;
use serde_json::json;
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::Mutex,
};

const DEFAULT_TIMEOUT_SECS: u64 = 15;
const DEFAULT_VIEWPORT_WIDTH: u32 = 1280;
const DEFAULT_VIEWPORT_HEIGHT: u32 = 800;

/// Configuration for the session-scoped headless Chrome/Chromium controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserConfig {
    pub(crate) allow_local: bool,
    pub(crate) chrome_executable: Option<PathBuf>,
    pub(crate) headless: bool,
    pub(crate) default_timeout: Duration,
    pub(crate) viewport_width: u32,
    pub(crate) viewport_height: u32,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            allow_local: true,
            chrome_executable: None,
            headless: true,
            default_timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            viewport_width: DEFAULT_VIEWPORT_WIDTH,
            viewport_height: DEFAULT_VIEWPORT_HEIGHT,
        }
    }
}

impl BrowserConfig {
    /// Controls whether explicit loopback targets (`localhost`, `127.0.0.1`, `::1`) may be opened.
    #[must_use]
    pub fn with_allow_local(mut self, allow_local: bool) -> Self {
        self.allow_local = allow_local;
        self
    }

    /// Sets an explicit Chrome/Chromium executable path instead of auto-discovery.
    #[must_use]
    pub fn with_chrome_executable(mut self, path: impl Into<PathBuf>) -> Self {
        self.chrome_executable = Some(path.into());
        self
    }

    /// Overrides the default CDP action timeout.
    #[must_use]
    pub fn with_default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = timeout;
        self
    }

    /// Overrides the initial viewport dimensions.
    #[must_use]
    pub fn with_viewport(mut self, width: u32, height: u32) -> Self {
        self.viewport_width = width;
        self.viewport_height = height;
        self
    }
}

struct ChildProcessGuard {
    child: Child,
    _profile_dir: TempDir,
}

impl ChildProcessGuard {
    fn kill_now(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl Drop for ChildProcessGuard {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

pub(crate) struct ActiveBrowserSession {
    pub(crate) cdp: Arc<CdpSession>,
    process: Option<ChildProcessGuard>,
}

impl ActiveBrowserSession {
    async fn shutdown(mut self) {
        if let Some(mut process) = self.process.take() {
            process.kill_now();
            let _ = tokio::time::timeout(Duration::from_secs(2), process.child.wait()).await;
        }
        if let Ok(cdp) = Arc::try_unwrap(self.cdp) {
            cdp.close().await;
        }
    }
}

/// Shared session-scoped headless browser manager shared by `browser_tab` and `browser_network`.
pub struct BrowserSessionManager {
    config: BrowserConfig,
    state: Mutex<Option<ActiveBrowserSession>>,
}

impl std::fmt::Debug for BrowserSessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserSessionManager")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl BrowserSessionManager {
    /// Creates a new lazy browser session manager with the given configuration.
    #[must_use]
    pub fn new(config: BrowserConfig) -> Self {
        Self {
            config,
            state: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_cdp(config: BrowserConfig, cdp: CdpSession) -> Self {
        Self {
            config,
            state: Mutex::new(Some(ActiveBrowserSession {
                cdp: Arc::new(cdp),
                process: None,
            })),
        }
    }

    #[must_use]
    pub(crate) fn config(&self) -> &BrowserConfig {
        &self.config
    }

    /// Returns the active CDP session, returning an error if no tab has been opened yet and `auto_launch` is false.
    pub(crate) async fn session(&self, auto_launch: bool) -> Result<Arc<CdpSession>, String> {
        let mut guard = self.state.lock().await;
        if let Some(active) = guard.as_ref() {
            if !active.cdp.is_closed() {
                return Ok(Arc::clone(&active.cdp));
            }
            if let Some(dead) = guard.take() {
                dead.shutdown().await;
            }
        }
        if !auto_launch {
            return Err(
                "No active browser session. Call `browser_tab` with `action: \"open\"` first."
                    .to_owned(),
            );
        }
        let active = launch_headless_browser(&self.config).await?;
        let cdp = Arc::clone(&active.cdp);
        *guard = Some(active);
        Ok(cdp)
    }

    /// Closes and terminates the active browser session if running.
    pub async fn close(&self) -> bool {
        let mut guard = self.state.lock().await;
        let Some(active) = guard.take() else {
            return false;
        };
        active.shutdown().await;
        true
    }
}

impl Default for BrowserSessionManager {
    fn default() -> Self {
        Self::new(BrowserConfig::default())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevToolsTarget {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    web_socket_debugger_url: Option<String>,
}

async fn launch_headless_browser(config: &BrowserConfig) -> Result<ActiveBrowserSession, String> {
    let executable = discover_chrome_executable(config.chrome_executable.as_deref())?;
    let profile_dir = tempfile::Builder::new()
        .prefix("merry-browser-")
        .tempdir()
        .map_err(|e| format!("failed creating temporary browser profile dir: {e}"))?;

    let mut cmd = Command::new(&executable);
    if config.headless {
        cmd.arg("--headless=new");
    }
    cmd.arg("--remote-debugging-port=0")
        .arg("--remote-debugging-address=127.0.0.1")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-background-networking")
        .arg("--disable-gpu")
        .arg("--disable-dev-shm-usage")
        .arg(format!(
            "--window-size={},{}",
            config.viewport_width, config.viewport_height
        ))
        .arg(format!("--user-data-dir={}", profile_dir.path().display()))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "failed spawning Chrome/Chromium ({}): {e}",
            executable.display()
        )
    })?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Chrome/Chromium stderr pipe unavailable".to_owned())?;
    let ws_browser_url = match read_devtools_ws_url(stderr, config.default_timeout).await {
        Ok(url) => url,
        Err(err) => {
            let _ = child.start_kill();
            return Err(err);
        }
    };

    let guard = ChildProcessGuard {
        child,
        _profile_dir: profile_dir,
    };

    let page_ws_url =
        discover_or_create_page_ws_url(&ws_browser_url, config.default_timeout).await?;
    let (ws_reader, ws_writer) = connect_tcp_ws(&page_ws_url).await?;
    let cdp = Arc::new(CdpSession::from_ws(ws_reader, ws_writer));

    let _ = cdp
        .call("Page.enable", json!({}), config.default_timeout)
        .await?;
    let _ = cdp
        .call("Runtime.enable", json!({}), config.default_timeout)
        .await?;
    let _ = cdp
        .call("Network.enable", json!({}), config.default_timeout)
        .await?;
    let _ = cdp
        .call(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": config.viewport_width,
                "height": config.viewport_height,
                "deviceScaleFactor": 1,
                "mobile": false,
            }),
            config.default_timeout,
        )
        .await;

    Ok(ActiveBrowserSession {
        cdp,
        process: Some(guard),
    })
}

async fn read_devtools_ws_url(
    stderr: tokio::process::ChildStderr,
    timeout: Duration,
) -> Result<String, String> {
    let mut lines = BufReader::new(stderr).lines();
    let deadline = tokio::time::Instant::now() + timeout;
    let mut recent_lines = Vec::new();
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, lines.next_line()).await {
            Ok(Ok(Some(line))) => {
                if let Some(pos) = line.find("ws://") {
                    let ws_part = line[pos..].trim();
                    if !ws_part.is_empty() {
                        return Ok(ws_part.to_owned());
                    }
                }
                if recent_lines.len() < 20 {
                    recent_lines.push(line);
                }
            }
            Ok(Ok(None)) => break,
            Ok(Err(e)) => return Err(format!("error reading Chrome stderr: {e}")),
            Err(_) => {
                return Err("timed out waiting for Chrome DevTools listening port".to_owned());
            }
        }
    }
    Err(format!(
        "Chrome exited without reporting DevTools WebSocket URL: {}",
        recent_lines.join(" | ")
    ))
}

async fn discover_or_create_page_ws_url(
    browser_ws_url: &str,
    timeout: Duration,
) -> Result<String, String> {
    let parsed = url::Url::parse(browser_ws_url)
        .map_err(|e| format!("invalid Chrome DevTools URL {browser_ws_url:?}: {e}"))?;
    let port = parsed
        .port()
        .ok_or_else(|| "Chrome DevTools URL missing port".to_owned())?;
    let host = parsed.host_str().unwrap_or("127.0.0.1");

    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| format!("failed to build loopback HTTP client: {e}"))?;

    let list_url = format!("http://{host}:{port}/json/list");
    if let Ok(resp) = client.get(&list_url).send().await
        && let Ok(targets) = resp.json::<Vec<DevToolsTarget>>().await
        && let Some(page_ws) = targets
            .into_iter()
            .find(|t| t.kind == "page")
            .and_then(|t| t.web_socket_debugger_url)
    {
        return Ok(page_ws);
    }

    let new_url = format!("http://{host}:{port}/json/new?about:blank");
    if let Ok(resp) = client.put(&new_url).send().await
        && let Ok(target) = resp.json::<DevToolsTarget>().await
        && let Some(page_ws) = target.web_socket_debugger_url
    {
        return Ok(page_ws);
    }

    Ok(browser_ws_url.to_owned())
}

fn discover_chrome_executable(explicit: Option<&std::path::Path>) -> Result<PathBuf, String> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(format!(
            "configured Chrome executable does not exist: {}",
            path.display()
        ));
    }
    if let Some(env_path) = std::env::var_os("MERRY_CHROME_PATH") {
        let path = PathBuf::from(env_path);
        if path.is_file() {
            return Ok(path);
        }
    }
    for candidate in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "chrome",
        "msedge",
    ] {
        if let Some(found) = find_in_path(candidate) {
            return Ok(found);
        }
    }
    for abs in [
        "/usr/bin/google-chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
    ] {
        let p = PathBuf::from(abs);
        if p.is_file() {
            return Ok(p);
        }
    }
    #[cfg(windows)]
    if let Some(found) = find_windows_browser_binary() {
        return Ok(found);
    }
    Err(
        "No headless Chrome or Chromium executable found (checked MERRY_CHROME_PATH, PATH, and standard system paths)."
            .to_owned(),
    )
}

#[cfg(windows)]
fn find_windows_browser_binary() -> Option<PathBuf> {
    let roots = [
        std::env::var_os("PROGRAMFILES"),
        std::env::var_os("PROGRAMFILES(X86)"),
        std::env::var_os("LOCALAPPDATA"),
    ];
    let relative_candidates = [
        r"Google\Chrome\Application\chrome.exe",
        r"Chromium\Application\chrome.exe",
        r"Microsoft\Edge\Application\msedge.exe",
    ];
    for root in roots.into_iter().flatten() {
        let base = PathBuf::from(root);
        for rel in relative_candidates {
            let candidate = base.join(rel);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn find_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    merry_core::host_paths::find_executable_in_path(&path_var, &[binary], std::path::Path::is_file)
}
