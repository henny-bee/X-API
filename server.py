"""
X poster backend: posts text + image/video to X using a saved browser login session.

n8n  --HTTP POST-->  this service  --Playwright (saved session)-->  x.com

Endpoints:
  GET  /health
  POST /post          JSON: {"text": "...", "media_urls": ["https://..."]}
  POST /post/upload   multipart: text=..., files=<binary> (repeat for multiple)

Auth: header  x-token: <API_TOKEN>
"""
import ipaddress
import logging
import os
import secrets
import socket
import tempfile
import threading
import time
from pathlib import Path
from typing import List, Optional
from urllib.parse import urlparse

import httpx
from fastapi import FastAPI, File, Form, Header, HTTPException, UploadFile
from pydantic import BaseModel, Field
from playwright.sync_api import TimeoutError as PWTimeout
from playwright.sync_api import sync_playwright

# ---------------------------------------------------------------- config
API_TOKEN = os.environ.get("API_TOKEN", "")
STATE_FILE = Path(os.environ.get("STATE_FILE", "./data/storage_state.json"))
DEBUG_DIR = Path(os.environ.get("DEBUG_DIR", "./data/debug"))
HEADLESS = os.environ.get("HEADLESS", "true").lower() != "false"
MIN_INTERVAL = int(os.environ.get("MIN_INTERVAL_SECONDS", "60"))
UPLOAD_TIMEOUT = int(os.environ.get("UPLOAD_TIMEOUT_SECONDS", "300"))
MAX_MEDIA_MB = int(os.environ.get("MAX_MEDIA_MB", "512"))
MAX_TEXT_CHARS = int(os.environ.get("MAX_TEXT_CHARS", "280"))
ALLOW_PRIVATE_MEDIA_URLS = os.environ.get("ALLOW_PRIVATE_MEDIA_URLS", "false").lower() == "true"

if not API_TOKEN:
    raise RuntimeError("Set API_TOKEN env var (long random string).")

# Selectors live in one place: if X changes its UI, fix them here.
COMPOSE_URL = "https://x.com/compose/post"
TEXTAREA = '[data-testid="tweetTextarea_0"]'
FILE_INPUT = 'input[data-testid="fileInput"]'
ATTACHMENTS = '[data-testid="attachments"]'
POST_BTN = '[data-testid="tweetButtonInline"]'
TOAST_LINK = '[data-testid="toast"] a'

IMAGE_EXT = {".jpg", ".jpeg", ".png", ".webp"}
GIF_EXT = {".gif"}
VIDEO_EXT = {".mp4", ".mov"}
ALLOWED_EXT = IMAGE_EXT | GIF_EXT | VIDEO_EXT
EXT_BY_CONTENT_TYPE = {
    "image/jpeg": ".jpg",
    "image/png": ".png",
    "image/webp": ".webp",
    "image/gif": ".gif",
    "video/mp4": ".mp4",
    "video/quicktime": ".mov",
}

log = logging.getLogger("x-poster")
logging.basicConfig(level=logging.INFO)
app = FastAPI(title="X poster")


# ---------------------------------------------------------------- helpers
def _auth(x_token: Optional[str]) -> None:
    if not x_token or not secrets.compare_digest(x_token, API_TOKEN):
        raise HTTPException(401, "unauthorized")


def _check_public_url(url: str) -> None:
    u = urlparse(url)
    if u.scheme not in ("http", "https") or not u.hostname:
        raise HTTPException(400, f"invalid media url: {url}")
    if ALLOW_PRIVATE_MEDIA_URLS:
        return
    try:
        infos = socket.getaddrinfo(u.hostname, None)
    except socket.gaierror:
        raise HTTPException(400, f"cannot resolve media host: {u.hostname}")
    for info in infos:
        ip = ipaddress.ip_address(info[4][0])
        if ip.is_private or ip.is_loopback or ip.is_link_local or ip.is_reserved or ip.is_multicast:
            raise HTTPException(400, "media url must point to a public host")


def _ext_for(url: str, content_type: str) -> str:
    ext = Path(urlparse(url).path).suffix.lower()
    if ext in ALLOWED_EXT:
        return ext
    return EXT_BY_CONTENT_TYPE.get(content_type.split(";")[0].strip().lower(), "")


def download(url: str, dest_dir: str) -> Path:
    """Download media to a temp dir with SSRF checks and a size cap."""
    max_bytes = MAX_MEDIA_MB * 1024 * 1024
    try:
        for _ in range(4):  # follow a few redirects manually, re-checking each hop
            _check_public_url(url)
            with httpx.stream("GET", url, timeout=60, follow_redirects=False) as r:
                if r.status_code in (301, 302, 303, 307, 308):
                    url = str(httpx.URL(url).join(r.headers["location"]))
                    continue
                r.raise_for_status()
                ext = _ext_for(url, r.headers.get("content-type", ""))
                if ext not in ALLOWED_EXT:
                    raise HTTPException(415, f"unsupported media type for {url}")
                dest = Path(dest_dir) / f"{secrets.token_hex(6)}{ext}"
                size = 0
                with open(dest, "wb") as f:
                    for chunk in r.iter_bytes(1 << 20):
                        size += len(chunk)
                        if size > max_bytes:
                            raise HTTPException(413, f"media larger than {MAX_MEDIA_MB} MB")
                        f.write(chunk)
                return dest
    except httpx.HTTPError as e:
        raise HTTPException(502, f"failed to download media: {e}")
    raise HTTPException(400, "too many redirects")


def validate(text: str, media: List[Path]) -> None:
    if not text.strip() and not media:
        raise HTTPException(400, "need text and/or media")
    if len(text) > MAX_TEXT_CHARS:
        raise HTTPException(400, f"text longer than {MAX_TEXT_CHARS} chars")
    if not media:
        return
    exts = [m.suffix.lower() for m in media]
    images = [e for e in exts if e in IMAGE_EXT]
    others = [e for e in exts if e not in IMAGE_EXT]
    if others and len(media) > 1:
        raise HTTPException(400, "a video or gif must be posted alone")
    if len(images) > 4:
        raise HTTPException(400, "max 4 images per post")


def _snap(page, name: str) -> None:
    try:
        DEBUG_DIR.mkdir(parents=True, exist_ok=True)
        page.screenshot(path=str(DEBUG_DIR / f"{int(time.time())}-{name}.png"))
    except Exception:  # noqa: BLE001
        pass


# ---------------------------------------------------------------- browser
_lock = threading.Lock()
_last_post = 0.0


def _run_browser(text: str, media: List[Path]) -> dict:
    if not STATE_FILE.exists():
        raise HTTPException(503, "no saved session. Run login.py first.")
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=HEADLESS)
        ctx = browser.new_context(
            storage_state=str(STATE_FILE),
            viewport={"width": 1280, "height": 900},
        )
        page = ctx.new_page()
        try:
            page.goto(COMPOSE_URL, wait_until="domcontentloaded")
            try:
                page.wait_for_selector(TEXTAREA, timeout=25000)
            except PWTimeout:
                _snap(page, "compose-not-found")
                if "login" in page.url or "logout" in page.url:
                    raise HTTPException(401, "session expired. Run login.py again.")
                raise HTTPException(502, "compose box not found (UI changed or blocked)")

            if text.strip():
                page.click(TEXTAREA)
                page.keyboard.insert_text(text)

            if media:
                page.set_input_files(FILE_INPUT, [str(m) for m in media])
                page.wait_for_selector(ATTACHMENTS, timeout=60000)

            # Post button becomes enabled only after text/media finished processing.
            page.wait_for_function(
                """(sel) => {
                    const b = document.querySelector(sel);
                    return b && b.getAttribute('aria-disabled') !== 'true' && !b.disabled;
                }""",
                arg=POST_BTN,
                timeout=UPLOAD_TIMEOUT * 1000,
            )
            page.click(POST_BTN)

            try:
                page.wait_for_selector(TEXTAREA, state="detached", timeout=30000)
            except PWTimeout:
                # Don't raise an error here: the post may have gone through,
                # and an error would make n8n retry and create a duplicate.
                _snap(page, "unconfirmed")
                log.warning("post not confirmed")
                return {"ok": None, "status": "unconfirmed", "url": None}

            post_url = None
            try:
                href = page.get_attribute(TOAST_LINK, "href", timeout=5000)
                if href:
                    post_url = href if href.startswith("http") else f"https://x.com{href}"
            except Exception:  # noqa: BLE001
                pass

            ctx.storage_state(path=str(STATE_FILE))  # keep session cookies fresh
            return {"ok": True, "status": "posted", "url": post_url}
        except PWTimeout:
            _snap(page, "timeout")
            raise HTTPException(504, "timed out while posting")
        finally:
            ctx.close()
            browser.close()


def publish(text: str, media: List[Path]) -> dict:
    global _last_post
    if not _lock.acquire(blocking=False):
        raise HTTPException(409, "another post is in progress")
    try:
        wait = MIN_INTERVAL - (time.time() - _last_post)
        if wait > 0:
            raise HTTPException(
                429, f"too soon, retry in {int(wait) + 1}s",
                headers={"Retry-After": str(int(wait) + 1)},
            )
        result = _run_browser(text, media)
        if result.get("ok") is not False:
            _last_post = time.time()
        return result
    finally:
        _lock.release()


# ---------------------------------------------------------------- API
class PostIn(BaseModel):
    text: str = ""
    media_urls: List[str] = Field(default_factory=list)


@app.get("/health")
def health():
    return {"ok": True, "session_saved": STATE_FILE.exists()}


@app.post("/post")
def post_json(body: PostIn, x_token: Optional[str] = Header(None)):
    _auth(x_token)
    with tempfile.TemporaryDirectory() as tmp:
        media = [download(u, tmp) for u in body.media_urls]
        validate(body.text, media)
        return publish(body.text, media)


@app.post("/post/upload")
def post_upload(
    text: str = Form(""),
    files: List[UploadFile] = File(default=[]),
    x_token: Optional[str] = Header(None),
):
    _auth(x_token)
    max_bytes = MAX_MEDIA_MB * 1024 * 1024
    with tempfile.TemporaryDirectory() as tmp:
        media: List[Path] = []
        for f in files:
            ext = Path(f.filename or "").suffix.lower() or EXT_BY_CONTENT_TYPE.get(f.content_type or "", "")
            if ext not in ALLOWED_EXT:
                raise HTTPException(415, f"unsupported file type: {f.filename}")
            dest = Path(tmp) / f"{secrets.token_hex(6)}{ext}"
            size = 0
            with open(dest, "wb") as out:
                while chunk := f.file.read(1 << 20):
                    size += len(chunk)
                    if size > max_bytes:
                        raise HTTPException(413, f"file larger than {MAX_MEDIA_MB} MB")
                    out.write(chunk)
            media.append(dest)
        validate(text, media)
        return publish(text, media)
