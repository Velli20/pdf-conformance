//! Headless Chrome, driven over the DevTools protocol, for the renderers that run in a browser:
//! Safe-PDF's browser stack (`pdf-web` on a Canvas 2D, with its annotation layer) and pdf.js.
//!
//! A run starts one [`WebHost`]: it builds the `web-canvas` WebAssembly package when Safe-PDF
//! renders in the browser, serves the harness pages and their files on a loopback port, and
//! launches the Chrome installed on the machine. Nothing is downloaded. Each page worker then
//! opens its own tab with [`screenshot`], renders the page there and screenshots it.

use crate::{
    corpus,
    process::{STAGE_MARKER, TARGET_MARKER},
};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use image::RgbaImage;
use serde_json::{Value, json};
use std::{
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{Message, WebSocket, protocol::WebSocketConfig, stream::MaybeTlsStream};

/// Worker stage while Safe-PDF renders the page in the browser.
pub const WEB_RENDER_STAGE: &str = "safe-web-render";

/// The Safe-PDF harness page, served at `/`.
const HARNESS: &str = include_str!("web_harness.html");

/// The pdf.js harness page, served at `/pdfjs/`.
const PDFJS_HARNESS: &str = include_str!("pdfjs_harness.html");

/// Largest DevTools message accepted; screenshots of large pages arrive as one message.
const MESSAGE_LIMIT: usize = 512 * 1024 * 1024;

/// How long Chrome may take to start listening.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Chrome executables looked up on `PATH`.
const CHROME_NAMES: [&str; 4] = [
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
];

/// Chrome's location in a macOS application install.
const CHROME_MAC: &str = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

/// Where page workers find the browser and the harness pages.
#[derive(Clone, Debug)]
pub struct WebEndpoints {
    /// `host:port` of Chrome's DevTools HTTP endpoint.
    pub devtools: String,
    /// Base URL of the harness server, ending in `/`.
    pub server: String,
}

impl WebEndpoints {
    /// Returns the URL of the Safe-PDF harness page.
    pub fn safe_pdf_page(&self) -> String {
        self.server.clone()
    }

    /// Returns the URL of the pdf.js harness page.
    pub fn pdfjs_page(&self) -> String {
        format!("{}pdfjs/", self.server)
    }
}

/// What the harness server offers besides its pages.
pub struct Sites {
    /// Builds and serves the `web-canvas` package, for Safe-PDF rendering in the browser.
    pub safe_pdf: bool,
    /// Unpacked `pdfjs-dist` package served under `/pdfjs/`.
    pub pdfjs: Option<PathBuf>,
}

/// The browser and harness server shared by the page workers of one run.
pub struct WebHost {
    chrome: PathBuf,
    server: String,
    browser: Mutex<Browser>,
}

impl WebHost {
    /// Builds what `sites` needs, starts the harness server and launches Chrome: `chrome`,
    /// else the first Chrome found in the usual places.
    pub fn start(chrome: Option<PathBuf>, sites: Sites) -> Result<Self> {
        let chrome = find_chrome(chrome)?;
        let files = Files {
            pkg: sites.safe_pdf.then(build_package).transpose()?,
            pdfjs: sites.pdfjs,
        };
        let server = serve(files)?;
        let browser = Browser::launch(&chrome)?;
        Ok(Self {
            chrome,
            server,
            browser: Mutex::new(browser),
        })
    }

    /// Returns the endpoints for the next page, relaunching Chrome if it stopped answering.
    pub fn endpoints(&self) -> Result<WebEndpoints> {
        let mut browser = self
            .browser
            .lock()
            .map_err(|_| anyhow!("browser lock poisoned"))?;
        if http(&browser.devtools, "GET", "/json/version").is_err() {
            println!("Chrome stopped answering; relaunching it");
            *browser = Browser::launch(&self.chrome)?;
        }
        Ok(WebEndpoints {
            devtools: browser.devtools.clone(),
            server: self.server.clone(),
        })
    }

    /// Closes the tab a killed worker named on its stderr, so it stops using the browser.
    pub fn close_tab(&self, stderr: &str) {
        let Some(id) = stderr
            .lines()
            .rev()
            .find_map(|line| line.strip_prefix(TARGET_MARKER))
        else {
            return;
        };
        if let Ok(browser) = self.browser.lock() {
            // A tab that already closed with its renderer process leaves nothing to do.
            let _ = http(
                &browser.devtools,
                "GET",
                &format!("/json/close/{}", id.trim()),
            );
        }
    }
}

/// Renders `page` of the document with Safe-PDF in a new tab at `width` x `height` pixels and
/// returns the screenshot with the milliseconds Safe-PDF took in the browser.
///
/// A Safe-PDF error inside the browser is returned as the error, with the message the
/// WebAssembly module reported.
pub fn capture(
    endpoints: &WebEndpoints,
    bytes: &[u8],
    password: Option<&str>,
    page: usize,
    width: u32,
    height: u32,
) -> Result<(RgbaImage, u128)> {
    let render = format!(
        "conformanceRender({}, {}, {page}, {width}, {height})",
        json!(BASE64.encode(bytes)),
        json!(password),
    );
    screenshot(
        endpoints,
        &endpoints.safe_pdf_page(),
        &render,
        WEB_RENDER_STAGE,
        (width, height),
    )
}

/// Opens `page_url` in a new tab sized `width` x `height`, waits for its
/// `conformanceReady` promise, evaluates `render` in worker stage `stage` and screenshots the
/// tab. Returns the screenshot with the milliseconds `render` took.
///
/// `render` resolves to an object; an `error` member is returned as the error.
pub fn screenshot(
    endpoints: &WebEndpoints,
    page_url: &str,
    render: &str,
    stage: &str,
    (width, height): (u32, u32),
) -> Result<(RgbaImage, u128)> {
    eprintln!("{STAGE_MARKER}web-open");
    let tab = Tab::open(&endpoints.devtools)?;
    let mut session = Session::connect(&tab.socket)?;
    session.call(
        "Emulation.setDeviceMetricsOverride",
        json!({ "width": width, "height": height, "deviceScaleFactor": 1, "mobile": false }),
    )?;
    session.call("Page.enable", json!({}))?;
    session.call("Page.navigate", json!({ "url": page_url }))?;
    session.wait_for("Page.loadEventFired")?;
    session.evaluate("window.conformanceReady.then(() => true)")?;

    eprintln!("{STAGE_MARKER}{stage}");
    let started = Instant::now();
    let rendered = session.evaluate(render)?;
    let elapsed = started.elapsed().as_millis();
    if let Some(error) = rendered.get("error").and_then(Value::as_str) {
        bail!("{error}");
    }

    eprintln!("{STAGE_MARKER}web-capture");
    let shot = session.call(
        "Page.captureScreenshot",
        json!({
            "format": "png",
            "clip": { "x": 0, "y": 0, "width": width, "height": height, "scale": 1 },
        }),
    )?;
    let data = shot
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("screenshot without data"))?;
    let image =
        image::load_from_memory_with_format(&BASE64.decode(data)?, image::ImageFormat::Png)?
            .to_rgba8();
    if image.dimensions() != (width, height) {
        bail!(
            "browser screenshot is {:?}, expected {:?}",
            image.dimensions(),
            (width, height)
        );
    }
    Ok((image, elapsed))
}

/// Returns `explicit`, the macOS application install, or the first Chrome on `PATH`.
fn find_chrome(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if !path.is_file() {
            bail!("Chrome {} not found", path.display());
        }
        return Ok(path);
    }
    let mac = Path::new(CHROME_MAC);
    if mac.is_file() {
        return Ok(mac.to_owned());
    }
    let path = env::var_os("PATH").unwrap_or_default();
    env::split_paths(&path)
        .flat_map(|dir| CHROME_NAMES.map(|name| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            anyhow!(
                "no Chrome found for the web renderer: install Google Chrome or Chromium, or pass \
                 --chrome / CHROME_PATH (nothing is downloaded)"
            )
        })
}

/// Builds the optimized `web-canvas` package and returns its directory.
fn build_package() -> Result<PathBuf> {
    let root = corpus::safe_pdf_root();
    println!("Building the web-canvas WebAssembly package");
    let status = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(&root)
        .args(["xtask", "web-canvas", "--profile", "release"])
        .status()
        .context("running cargo xtask web-canvas")?;
    if !status.success() {
        bail!("cargo xtask web-canvas failed ({status})");
    }
    Ok(root.join("examples").join("web-canvas").join("pkg"))
}

/// Directories the harness server reads files from.
struct Files {
    /// The built `web-canvas` package, served flat under `/pkg/`.
    pkg: Option<PathBuf>,
    /// The unpacked `pdfjs-dist` package, served with its subdirectories under `/pdfjs/`.
    pdfjs: Option<PathBuf>,
}

impl Files {
    /// Returns the file a request path names, if it names one this server offers.
    fn resolve(&self, path: &str) -> Option<PathBuf> {
        let safe = |part: &str| !part.is_empty() && !part.starts_with('.') && !part.contains('\\');
        if let Some(name) = path.strip_prefix("/pkg/") {
            return (safe(name) && !name.contains('/'))
                .then(|| self.pkg.as_ref().map(|pkg| pkg.join(name)))
                .flatten();
        }
        let relative = path.strip_prefix("/pdfjs/")?;
        let root = self.pdfjs.as_ref()?;
        relative.split('/').all(safe).then(|| {
            relative
                .split('/')
                .fold(root.clone(), |dir, part| dir.join(part))
        })
    }
}

/// Serves the harness pages and `files` on a loopback port; returns the server's base URL.
fn serve(files: Files) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/", listener.local_addr()?);
    let files = Arc::new(files);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let files = Arc::clone(&files);
            thread::spawn(move || {
                // A browser that drops a connection only loses that response.
                let _ = respond(stream, &files);
            });
        }
    });
    Ok(url)
}

/// Answers one request: `/` and `/pdfjs/` with the harness pages, other paths with the file
/// [`Files::resolve`] finds.
fn respond(mut stream: TcpStream, files: &Files) -> Result<()> {
    let mut reader = BufReader::new(&stream);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    let mut header = String::new();
    while reader.read_line(&mut header)? > 2 {
        header.clear();
    }
    let target = request.split_whitespace().nth(1).unwrap_or_default();
    let path = target.split(['?', '#']).next().unwrap_or_default();
    let not_found = || ("404 Not Found", "text/plain", b"not found".to_vec());
    let (status, content_type, body) = match path {
        "/" => ("200 OK", "text/html", HARNESS.as_bytes().to_vec()),
        "/pdfjs/" => ("200 OK", "text/html", PDFJS_HARNESS.as_bytes().to_vec()),
        _ => match files.resolve(path).map(|file| (fs::read(&file), file)) {
            Some((Ok(body), file)) => ("200 OK", content_type(&file), body),
            _ => not_found(),
        },
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(&body)?;
    Ok(())
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|extension| extension.to_str()) {
        Some("wasm") => "application/wasm",
        Some("js" | "mjs") => "text/javascript",
        Some("json") => "application/json",
        Some("css") => "text/css",
        Some("html") => "text/html",
        _ => "application/octet-stream",
    }
}

/// Sends one HTTP request to `address` and returns the body of a 200 response.
fn http(address: &str, method: &str, path: &str) -> Result<String> {
    let mut stream = TcpStream::connect(address)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )?;
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status)?;
    let mut length = None;
    let mut header = String::new();
    while reader.read_line(&mut header)? > 2 {
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<u64>().ok();
        }
        header.clear();
    }
    if !status.contains(" 200") {
        bail!("{method} {path}: {}", status.trim());
    }
    let mut body = String::new();
    match length {
        Some(length) => reader.take(length).read_to_string(&mut body)?,
        None => reader.read_to_string(&mut body)?,
    };
    Ok(body)
}

/// A headless Chrome with its own temporary profile; killed when dropped.
struct Browser {
    child: Child,
    profile: PathBuf,
    devtools: String,
}

impl Browser {
    fn launch(chrome: &Path) -> Result<Self> {
        static LAUNCHES: AtomicUsize = AtomicUsize::new(0);
        let profile = env::temp_dir().join(format!(
            "safe-pdf-conformance-chrome-{}-{}",
            std::process::id(),
            LAUNCHES.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&profile);
        fs::create_dir_all(&profile)?;
        let mut command = Command::new(chrome);
        command
            .args([
                "--headless=new",
                "--disable-gpu",
                "--hide-scrollbars",
                "--force-color-profile=srgb",
                "--font-render-hinting=none",
                "--disable-lcd-text",
                "--disable-extensions",
                "--no-first-run",
                "--no-default-browser-check",
                "--remote-debugging-port=0",
                "about:blank",
            ])
            .arg(format!("--user-data-dir={}", profile.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Containers often cannot start Chrome's sandbox; CI opts out explicitly.
        if env::var_os("CONFORMANCE_CHROME_NO_SANDBOX").is_some() {
            command.arg("--no-sandbox");
        }
        let child = command
            .spawn()
            .with_context(|| format!("starting {}", chrome.display()))?;
        let mut browser = Self {
            child,
            profile,
            devtools: String::new(),
        };
        browser.devtools = browser.wait_for_port()?;
        Ok(browser)
    }

    /// Waits for Chrome to write the DevTools port into its profile.
    fn wait_for_port(&mut self) -> Result<String> {
        let started = Instant::now();
        let file = self.profile.join("DevToolsActivePort");
        while started.elapsed() < LAUNCH_TIMEOUT {
            if let Some(status) = self.child.try_wait()? {
                bail!("Chrome exited during startup ({status})");
            }
            if let Some(port) = fs::read_to_string(&file)
                .ok()
                .and_then(|text| text.lines().next().map(str::to_owned))
                .filter(|port| !port.is_empty())
            {
                return Ok(format!("127.0.0.1:{port}"));
            }
            thread::sleep(Duration::from_millis(50));
        }
        bail!("Chrome did not open its DevTools port within {LAUNCH_TIMEOUT:?}")
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.profile);
    }
}

/// A browser tab; closed when dropped.
struct Tab {
    devtools: String,
    id: String,
    socket: String,
}

impl Tab {
    /// Opens a blank tab and names it on stderr for the harness.
    fn open(devtools: &str) -> Result<Self> {
        let created: Value =
            serde_json::from_str(&http(devtools, "PUT", "/json/new?about:blank")?)?;
        let field = |name: &str| {
            created
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("new tab without `{name}`"))
        };
        let tab = Self {
            devtools: devtools.to_owned(),
            id: field("id")?,
            socket: field("webSocketDebuggerUrl")?,
        };
        eprintln!("{TARGET_MARKER}{}", tab.id);
        Ok(tab)
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        let _ = http(&self.devtools, "GET", &format!("/json/close/{}", self.id));
    }
}

/// A DevTools protocol connection to one tab.
struct Session {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
    /// Events that arrived while waiting for a command's result.
    events: Vec<String>,
}

impl Session {
    fn connect(url: &str) -> Result<Self> {
        let config = WebSocketConfig::default()
            .max_message_size(Some(MESSAGE_LIMIT))
            .max_frame_size(Some(MESSAGE_LIMIT));
        let (socket, _) = tungstenite::client::connect_with_config(url, Some(config), 0)
            .with_context(|| format!("connecting to {url}"))?;
        Ok(Self {
            socket,
            next_id: 1,
            events: Vec::new(),
        })
    }

    /// Reads the next protocol message, skipping frames that carry none.
    fn read(&mut self) -> Result<Value> {
        loop {
            if let Message::Text(text) = self.socket.read()? {
                return Ok(serde_json::from_str(text.as_str())?);
            }
        }
    }

    /// Sends a command and returns its result, skipping events that arrive meanwhile.
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let request = json!({ "id": id, "method": method, "params": params });
        self.socket.send(Message::text(request.to_string()))?;
        loop {
            let mut message = self.read()?;
            if let Some(event) = message.get("method").and_then(Value::as_str) {
                self.events.push(event.to_owned());
                continue;
            }
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                bail!("{method}: {error}");
            }
            return Ok(message
                .get_mut("result")
                .map(Value::take)
                .unwrap_or_default());
        }
    }

    /// Waits for the event `method`, which may have arrived during an earlier command.
    fn wait_for(&mut self, method: &str) -> Result<()> {
        if self.events.iter().any(|event| event == method) {
            self.events.clear();
            return Ok(());
        }
        while self.read()?.get("method").and_then(Value::as_str) != Some(method) {}
        Ok(())
    }

    /// Evaluates `expression` in the page, awaiting a promise, and returns its value.
    fn evaluate(&mut self, expression: &str) -> Result<Value> {
        let mut result = self.call(
            "Runtime.evaluate",
            json!({ "expression": expression, "awaitPromise": true, "returnByValue": true }),
        )?;
        if let Some(details) = result.get("exceptionDetails") {
            let message = details
                .pointer("/exception/description")
                .or_else(|| details.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("exception");
            bail!("harness script failed: {message}");
        }
        Ok(result
            .pointer_mut("/result/value")
            .map(Value::take)
            .unwrap_or_default())
    }
}
