use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

#[derive(Debug)]
pub enum CdpError {
    Timeout,
    Launch(String),
    Protocol(String),
}

impl std::fmt::Display for CdpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "operation timed out"),
            Self::Launch(msg) => write!(f, "browser launch error: {msg}"),
            Self::Protocol(msg) => write!(f, "cdp error: {msg}"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StorageCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    #[serde(default = "default_path")]
    pub path: String,
    #[serde(default)]
    pub expires: f64,
    #[serde(default, rename = "httpOnly")]
    pub http_only: bool,
    #[serde(default)]
    pub secure: bool,
    #[serde(default, rename = "sameSite", skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}

fn default_path() -> String {
    "/".into()
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StorageState {
    #[serde(default)]
    pub cookies: Vec<StorageCookie>,
    #[serde(default)]
    pub origins: Vec<Value>,
}

#[derive(Deserialize)]
struct TargetInfo {
    #[serde(rename = "type")]
    target_type: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    ws_url: Option<String>,
}

pub struct CdpSession {
    child: Child,
    _profile_dir: TempDir,
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl CdpSession {
    pub async fn launch(
        chrome_bin: Option<&str>,
        headless: bool,
        no_sandbox: bool,
    ) -> Result<Self, CdpError> {
        let bin = find_chrome_binary(chrome_bin)?;
        let profile_dir = TempDir::new().map_err(|e| CdpError::Launch(e.to_string()))?;

        let mut cmd = Command::new(&bin);
        cmd.arg("--remote-debugging-port=0")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            .arg("--disable-dev-shm-usage")
            .arg("--disable-background-networking")
            .arg("--disable-sync")
            .arg("--window-size=1280,900")
            .arg(format!("--user-data-dir={}", profile_dir.path().display()));

        if headless {
            cmd.arg("--headless=new").arg("--disable-gpu");
        }
        if no_sandbox {
            cmd.arg("--no-sandbox");
        }

        cmd.arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| CdpError::Launch(format!("failed to spawn {bin}: {e}")))?;

        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| CdpError::Launch("missing browser stderr".into()))?;

        let browser_ws = read_devtools_ws(stderr).await?;
        let host_port = extract_host_port(&browser_ws)?;
        let page_ws = find_page_ws(&host_port).await?;

        let (ws, _) = connect_async(&page_ws)
            .await
            .map_err(|e| CdpError::Launch(format!("websocket connect failed: {e}")))?;

        let mut session = Self {
            child,
            _profile_dir: profile_dir,
            ws,
            next_id: 1,
        };

        session.call("Page.enable", json!({})).await?;
        session.call("Runtime.enable", json!({})).await?;
        session.call("DOM.enable", json!({})).await?;
        session.call("Network.enable", json!({})).await?;
        if headless {
            session
                .call(
                    "Emulation.setDeviceMetricsOverride",
                    json!({
                        "width": 1280,
                        "height": 900,
                        "deviceScaleFactor": 1,
                        "mobile": false
                    }),
                )
                .await?;
        }

        Ok(session)
    }

    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, CdpError> {
        let id = self.next_id;
        self.next_id += 1;

        let payload = json!({
            "id": id,
            "method": method,
            "params": params,
        });

        self.ws
            .send(Message::Text(payload.to_string().into()))
            .await
            .map_err(|e| CdpError::Protocol(e.to_string()))?;

        let deadline = tokio::time::sleep(Duration::from_secs(60));
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                _ = &mut deadline => return Err(CdpError::Timeout),
                msg = self.ws.next() => {
                    let msg = msg
                        .ok_or_else(|| CdpError::Protocol("browser websocket closed".into()))?
                        .map_err(|e| CdpError::Protocol(e.to_string()))?;

                    if let Message::Text(text) = msg {
                        let v: Value = serde_json::from_str(&text)
                            .map_err(|e| CdpError::Protocol(e.to_string()))?;
                        if v.get("id").and_then(Value::as_u64) == Some(id) {
                            if let Some(err) = v.get("error") {
                                return Err(CdpError::Protocol(err.to_string()));
                            }
                            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                        }
                    }
                }
            }
        }
    }

    pub async fn set_cookies(&mut self, cookies: &[StorageCookie]) -> Result<(), CdpError> {
        let cdp_cookies: Vec<Value> = cookies
            .iter()
            .map(|c| {
                let mut obj = json!({
                    "name": c.name,
                    "value": c.value,
                    "domain": c.domain,
                    "path": c.path,
                    "httpOnly": c.http_only,
                    "secure": c.secure,
                });
                if c.expires > 0.0 {
                    obj["expires"] = json!(c.expires);
                }
                if let Some(same_site) = &c.same_site {
                    if matches!(same_site.as_str(), "Strict" | "Lax" | "None") {
                        obj["sameSite"] = json!(same_site);
                    }
                }
                obj
            })
            .collect();

        self.call("Network.setCookies", json!({ "cookies": cdp_cookies }))
            .await?;
        Ok(())
    }

    pub async fn get_cookies(&mut self) -> Result<Vec<StorageCookie>, CdpError> {
        let res = self.call("Network.getAllCookies", json!({})).await?;
        let list = res
            .get("cookies")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        serde_json::from_value(list).map_err(|e| CdpError::Protocol(e.to_string()))
    }

    pub async fn goto(&mut self, url: &str) -> Result<(), CdpError> {
        self.call("Page.navigate", json!({ "url": url })).await?;
        self.wait_for_js(
            "document.readyState === 'interactive' || document.readyState === 'complete'",
            Duration::from_secs(30),
        )
        .await
    }

    pub async fn eval(&mut self, expression: &str) -> Result<Value, CdpError> {
        let res = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        Ok(res
            .get("result")
            .and_then(|r| r.get("value"))
            .cloned()
            .unwrap_or(Value::Null))
    }

    pub async fn wait_for_js(&mut self, expr: &str, timeout: Duration) -> Result<(), CdpError> {
        let start = Instant::now();
        loop {
            if self.eval(expr).await.ok().and_then(|v| v.as_bool()) == Some(true) {
                return Ok(());
            }
            if start.elapsed() >= timeout {
                return Err(CdpError::Timeout);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn wait_for_selector(
        &mut self,
        selector: &str,
        timeout: Duration,
    ) -> Result<(), CdpError> {
        let sel_json = serde_json::to_string(selector).unwrap_or_default();
        let expr = format!("Boolean(document.querySelector({sel_json}))");
        self.wait_for_js(&expr, timeout).await
    }

    pub async fn click(&mut self, selector: &str) -> Result<(), CdpError> {
        let sel_json = serde_json::to_string(selector).unwrap_or_default();
        let expr = format!(
            "(() => {{ const el = document.querySelector({sel_json}); if (!el) return false; el.focus(); el.click(); return true; }})()"
        );
        if self.eval(&expr).await?.as_bool() != Some(true) {
            return Err(CdpError::Protocol(format!("selector not found: {selector}")));
        }
        Ok(())
    }

    pub async fn insert_text(&mut self, text: &str) -> Result<(), CdpError> {
        self.call("Input.insertText", json!({ "text": text }))
            .await?;
        Ok(())
    }

    pub async fn set_input_files(
        &mut self,
        selector: &str,
        files: &[PathBuf],
    ) -> Result<(), CdpError> {
        self.wait_for_selector(selector, Duration::from_secs(15))
            .await?;
        let doc = self.call("DOM.getDocument", json!({ "depth": 0 })).await?;
        let root_id = doc
            .pointer("/root/nodeId")
            .and_then(Value::as_u64)
            .ok_or_else(|| CdpError::Protocol("missing DOM root nodeId".into()))?;

        let q = self
            .call(
                "DOM.querySelector",
                json!({
                    "nodeId": root_id,
                    "selector": selector,
                }),
            )
            .await?;
        let node_id = q
            .get("nodeId")
            .and_then(Value::as_u64)
            .filter(|&id| id != 0)
            .ok_or_else(|| CdpError::Protocol(format!("file input not found: {selector}")))?;

        let file_strs: Vec<String> = files
            .iter()
            .map(|p| {
                std::fs::canonicalize(p)
                    .unwrap_or_else(|_| p.clone())
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        self.call(
            "DOM.setFileInputFiles",
            json!({
                "nodeId": node_id,
                "files": file_strs,
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn current_url(&mut self) -> Result<String, CdpError> {
        Ok(self
            .eval("window.location.href")
            .await?
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    pub async fn get_attribute(
        &mut self,
        selector: &str,
        attr: &str,
        timeout: Duration,
    ) -> Result<Option<String>, CdpError> {
        let sel_json = serde_json::to_string(selector).unwrap_or_default();
        let attr_json = serde_json::to_string(attr).unwrap_or_default();
        let expr = format!("document.querySelector({sel_json})?.getAttribute({attr_json}) ?? null");
        let start = Instant::now();
        loop {
            if let Ok(val) = self.eval(&expr).await {
                if let Some(s) = val.as_str() {
                    return Ok(Some(s.to_string()));
                }
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn screenshot(&mut self, path: &Path) -> Result<(), CdpError> {
        let res = self
            .call("Page.captureScreenshot", json!({ "format": "png" }))
            .await?;
        let data = res
            .get("data")
            .and_then(Value::as_str)
            .ok_or_else(|| CdpError::Protocol("missing screenshot data".into()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| CdpError::Protocol(e.to_string()))?;
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        tokio::fs::write(path, bytes)
            .await
            .map_err(|e| CdpError::Protocol(e.to_string()))
    }
}

impl Drop for CdpSession {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn find_chrome_binary(configured: Option<&str>) -> Result<String, CdpError> {
    if let Some(bin) = configured {
        return Ok(bin.to_string());
    }
    let candidates = [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    ];
    for c in candidates {
        if Path::new(c).is_file_or_in_path(c) {
            return Ok(c.to_string());
        }
    }
    Err(CdpError::Launch(
        "Chromium/Chrome binary not found. Install chromium or set CHROME_BIN.".into(),
    ))
}

trait PathExt {
    fn is_file_or_in_path(&self, name: &str) -> bool;
}

impl PathExt for Path {
    fn is_file_or_in_path(&self, name: &str) -> bool {
        if self.is_file() {
            return true;
        }
        std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths).any(|dir| {
                    let full = dir.join(name);
                    full.is_file() || full.with_extension("exe").is_file()
                })
            })
            .unwrap_or(false)
    }
}

async fn read_devtools_ws(stderr: tokio::process::ChildStderr) -> Result<String, CdpError> {
    let mut lines = BufReader::new(stderr).lines();
    let deadline = tokio::time::sleep(Duration::from_secs(15));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => {
                return Err(CdpError::Launch("timed out waiting for DevTools websocket".into()));
            }
            line = lines.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        if let Some(idx) = l.find("ws://") {
                            return Ok(l[idx..].trim().to_string());
                        }
                    }
                    Ok(None) => {
                        return Err(CdpError::Launch("browser exited before opening DevTools port".into()));
                    }
                    Err(e) => return Err(CdpError::Launch(e.to_string())),
                }
            }
        }
    }
}

fn extract_host_port(ws_url: &str) -> Result<String, CdpError> {
    let parsed = url::Url::parse(ws_url).map_err(|e| CdpError::Launch(e.to_string()))?;
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    let port = parsed
        .port()
        .ok_or_else(|| CdpError::Launch("missing port in DevTools url".into()))?;
    Ok(format!("{host}:{port}"))
}

async fn find_page_ws(host_port: &str) -> Result<String, CdpError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| CdpError::Launch(e.to_string()))?;
    let list_url = format!("http://{host_port}/json/list");

    for _ in 0..20 {
        if let Ok(resp) = client.get(&list_url).send().await {
            if let Ok(targets) = resp.json::<Vec<TargetInfo>>().await {
                if let Some(ws) = targets
                    .into_iter()
                    .find(|t| t.target_type == "page")
                    .and_then(|t| t.ws_url)
                {
                    return Ok(ws);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    Err(CdpError::Launch("no page target found in browser".into()))
}
