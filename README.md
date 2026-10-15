# X Poster (Rust Backend)

HTTP service written in Rust (Axum + Chrome DevTools Protocol) that posts text, images, and videos to X (`x.com`) using a saved browser session cookie. Built to be called from n8n or any HTTP client.

```
n8n / HTTP Client -> x-poster (Axum/Rust) -> Headless Chromium (CDP) -> x.com
```

## Project Structure

```
.
├── Cargo.toml
├── Dockerfile
├── docker-compose.yml
└── src/
    ├── main.rs          # HTTP server entrypoint and graceful shutdown
    ├── lib.rs           # Module exports and integration tests
    ├── config.rs        # Environment variable configuration
    ├── error.rs         # HTTP error responses and Retry-After headers
    ├── state.rs         # Shared state, concurrency lock, and rate limiter
    ├── media.rs         # Media validation, SSRF-safe downloader, and size limits
    ├── browser/
    │   ├── mod.rs       # X compose automation and session persistence
    │   └── cdp.rs       # Chromium process manager and WebSocket CDP client
    └── routes/
        ├── mod.rs       # Axum router and x-token authentication
        ├── health.rs    # GET /health
        ├── session.rs   # POST /session and DELETE /session
        └── post.rs      # POST /post and POST /post/upload
```

## 1. Get Your X Session Cookies

1. Open `https://x.com` in your browser (an Incognito/Private window is recommended) and log in.
2. Open DevTools (`F12`) -> **Application** tab (**Storage** in Firefox) -> **Cookies** -> `https://x.com`.
3. Click each row below and copy the full **Value** string:
   - `auth_token` (required)
   - `ct0` (recommended)
   - Other cookies (`__cf_bm`, `cf_clearance`, `guest_id`, `twid`, `d_prefs`) are not needed.
4. Close the browser tab when done. Do not click **Log out**, because logging out invalidates the `auth_token` cookie on X's servers.

## 2. Run the Server and Save Your Session

### Start the Server

With Docker Compose:
```bash
echo "API_TOKEN=$(openssl rand -hex 32)" > .env
docker compose up -d --build
```

Or run locally with Cargo (requires Chromium or Chrome installed):
```bash
export API_TOKEN="$(openssl rand -hex 32)"
export HOST=127.0.0.1
export PORT=8001
cargo run --release
```

### Save Session via `POST /session`

Send your `auth_token` (and optional `ct0`) to `/session`:

```bash
curl -X POST http://127.0.0.1:8001/session \
  -H "x-token: $API_TOKEN" \
  -H "content-type: application/json" \
  -d '{"auth_token": "YOUR_AUTH_TOKEN", "ct0": "YOUR_CT0"}'
```

Or send a raw `Cookie` header string copied from DevTools:

```bash
curl -X POST http://127.0.0.1:8001/session \
  -H "x-token: $API_TOKEN" \
  -H "content-type: application/json" \
  -d '{"cookie": "auth_token=xxx; ct0=yyy"}'
```

The session is stored in `data/storage_state.json` and refreshed automatically after each successful post.

## 3. HTTP Endpoints

Every endpoint except `GET /health` requires the `x-token: <API_TOKEN>` header.

### `GET /health`
Returns server and session status:
```json
{ "ok": true, "session_saved": true }
```

### `POST /session`
Saves or updates the X session cookies (`auth_token` + optional `ct0`, raw `cookie` string, or `cookies` array).

### `DELETE /session`
Removes the saved `storage_state.json` file.

### `POST /post` (JSON with public media URLs)
```json
{
  "text": "Post caption",
  "media_urls": ["https://example.com/image.jpg"]
}
```

### `POST /post/upload` (multipart/form-data with binary files)
Form fields:
- `text` (Text): post caption
- `files` (File): binary media file (repeat the `files` field to attach up to 4 images)

Media rules enforced by X: up to 4 images (`.jpg`, `.jpeg`, `.png`, `.webp`), or 1 GIF (`.gif`), or 1 video (`.mp4`, `.mov`). Videos and GIFs cannot be combined with other attachments.

### `/post` Responses
- `200 OK`: `{"ok": true, "status": "posted", "url": "https://x.com/.../status/..."}` when confirmed.
- `200 OK`: `{"ok": null, "status": "unconfirmed", "url": null}` if the post button was clicked but completion could not be verified within 30 seconds. Check the account timeline before retrying.
- `401 Unauthorized`: invalid `x-token` or expired X session.
- `409 Conflict`: another post is currently running.
- `429 Too Many Requests`: minimum interval between posts has not elapsed (includes `Retry-After` header).
- `502 Bad Gateway`: X rejected the post (such as duplicate text) or the compose UI could not be loaded.
- `504 Gateway Timeout`: media processing or page navigation timed out.

## 4. n8n HTTP Request Node Configuration

| Field | Value |
|---|---|
| Method | `POST` |
| URL | `http://x-poster:8000/post` (same Compose network) or `http://host.docker.internal:8001/post` |
| Headers | `x-token` = `API_TOKEN` |
| Body (JSON) | `{{ JSON.stringify({ text: $json.caption, media_urls: [$json.media_url] }) }}` |
| Options > Timeout | `600000` |
| Settings > Retry On Fail | Disabled (prevents duplicate posts) |

To upload binary files from n8n, set Body Content Type to `Form-Data`, add `text` (Text) and `files` (n8n Binary File), and call `/post/upload`.

## 5. Environment Variables

| Variable | Default | Description |
|---|---|---|
| `API_TOKEN` | *(required)* | Secret token verified against the `x-token` request header |
| `HOST` | `0.0.0.0` | HTTP server bind address |
| `PORT` | `8000` | Internal HTTP server port |
| `HOST_PORT` | `8001` | Host port mapped by `docker-compose.yml` (`127.0.0.1:8001->8000`) |
| `STATE_FILE` | `./data/storage_state.json` | Path where session cookies are persisted |
| `DEBUG_DIR` | `./data/debug` | Directory where failure screenshots are saved |
| `HEADLESS` | `true` | Run Chromium in headless mode (`false` to show window) |
| `MIN_INTERVAL_SECONDS` | `60` | Minimum delay required between posts |
| `UPLOAD_TIMEOUT_SECONDS` | `300` | Maximum wait time for X to process uploaded media |
| `MAX_MEDIA_MB` | `512` | Maximum size per media file in megabytes |
| `MAX_TEXT_CHARS` | `280` | Maximum character count per post |
| `ALLOW_PRIVATE_MEDIA_URLS` | `false` | Set to `true` only when downloading media from private/internal hosts |
| `CHROME_BIN` | *(auto-detected)* | Custom path to the Chromium or Chrome binary |
| `CHROME_NO_SANDBOX` | `false` (`true` in Docker) | Pass `--no-sandbox` to Chromium inside containers |
