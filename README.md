# X Poster (Rust Backend)

Stateless HTTP service written in Rust (Axum + Chrome DevTools Protocol) that posts text, images, and videos to X (`x.com`). Each request supplies its own X session cookie (`auth_token` and optional `ct0`), so multiple users and X accounts can share one deployment.

<img width="1987" height="538" alt="image" src="https://github.com/user-attachments/assets/33b8fb4e-7214-43d9-9939-fb211efaba1f" />


```
Client -> x-poster (Axum/Rust) -> Headless Chromium (CDP) -> x.com
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
    ├── state.rs         # Per-account rate limiter and browser concurrency guard
    ├── media.rs         # Media validation, SSRF-safe downloader, and size limits
    ├── browser/
    │   ├── mod.rs       # X compose automation
    │   └── cdp.rs       # Chromium process manager and WebSocket CDP client
    └── routes/
        ├── mod.rs       # Axum router, x-token auth, and cookie extraction
        ├── health.rs    # GET /health
        └── post.rs      # POST /post and POST /post/upload
```

## 1. Get Your X Session Cookies

1. Open `https://x.com` in your browser (an Incognito/Private window is recommended) and log in.
2. Open DevTools (`F12`) -> **Application** tab (**Storage** in Firefox) -> **Cookies** -> `https://x.com`.
3. Click each row below and copy the full **Value** string:
   - `auth_token` (required)
   - `ct0` (optional, recommended)
   - Other cookies (`__cf_bm`, `cf_clearance`, `guest_id`, `twid`, `d_prefs`) are not needed.
4. Close the browser tab when done. Do not click **Log out**, because logging out invalidates the `auth_token` cookie on X's servers.

## 2. Run the Server

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

## 3. HTTP Endpoints

All `POST` requests require:
- `x-token: <API_TOKEN>` header for service authentication.
- The user's X session cookie passed either as headers (`x-auth-token` and optional `x-ct0`) or inside the JSON/form body (`auth_token` and optional `ct0`).

Rate limiting (`MIN_INTERVAL_SECONDS`) and active-post locks are tracked **per `auth_token`**, so different X accounts can post without blocking each other's cooldown.

### `GET /health`
Returns server status:
```json
{ "ok": true }
```

### `POST /post` (JSON with public media URLs)
Pass session cookies via headers (`x-auth-token`, `x-ct0`) or JSON fields (`auth_token`, `ct0`):
```bash
curl -X POST http://127.0.0.1:8001/post \
  -H "x-token: $API_TOKEN" \
  -H "x-auth-token: YOUR_AUTH_TOKEN" \
  -H "x-ct0: YOUR_CT0" \
  -H "content-type: application/json" \
  -d '{
    "text": "Post caption",
    "media_urls": ["https://example.com/image.jpg"]
  }'
```

### `POST /post/upload` (multipart/form-data with binary files)
Headers:
- `x-token: <API_TOKEN>`
- `x-auth-token: <YOUR_AUTH_TOKEN>` (or send as a `auth_token` form field)
- `x-ct0: <YOUR_CT0>` (optional, or send as a `ct0` form field)

Form fields:
- `text` (Text): post caption
- `files` (File): binary media file (repeat the `files` field to attach up to 4 images)

Media rules enforced by X: up to 4 images (`.jpg`, `.jpeg`, `.png`, `.webp`), or 1 GIF (`.gif`), or 1 video (`.mp4`, `.mov`). Videos and GIFs cannot be combined with other attachments.

### Responses
- `200 OK`: `{"ok": true, "status": "posted", "url": "https://x.com/.../status/..."}` when confirmed.
- `200 OK`: `{"ok": null, "status": "unconfirmed", "url": null}` if the post button was clicked but completion could not be verified within 30 seconds. Check the account timeline before retrying.
- `400 Bad Request`: missing `auth_token`, empty post, or invalid media rules.
- `401 Unauthorized`: invalid `x-token` or expired X `auth_token`.
- `409 Conflict`: another post is currently in progress for the same X account.
- `429 Too Many Requests`: minimum interval between posts for this X account has not elapsed (includes `Retry-After` header).
- `502 Bad Gateway`: X rejected the post (such as duplicate text) or the compose UI could not be loaded.
- `504 Gateway Timeout`: media processing or page navigation timed out.

## 4. n8n HTTP Request Node Configuration

| Field | Value |
|---|---|
| Method | `POST` |
| URL | `http://x-poster:8000/post` (same Compose network) or `http://host.docker.internal:8001/post` |
| Headers | `x-token` = `API_TOKEN`, `x-auth-token` = `{{ $json.auth_token }}`, `x-ct0` = `{{ $json.ct0 }}` |
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
| `DEBUG_DIR` | `./data/debug` | Directory where failure screenshots are saved |
| `HEADLESS` | `true` | Run Chromium in headless mode (`false` to show window) |
| `MIN_INTERVAL_SECONDS` | `60` | Minimum delay required between posts per X account |
| `UPLOAD_TIMEOUT_SECONDS` | `300` | Maximum wait time for X to process uploaded media |
| `MAX_MEDIA_MB` | `512` | Maximum size per media file in megabytes |
| `MAX_TEXT_CHARS` | `280` | Maximum character count per post |
| `ALLOW_PRIVATE_MEDIA_URLS` | `false` | Set to `true` only when downloading media from private/internal hosts |
| `CHROME_BIN` | *(auto-detected)* | Custom path to the Chromium or Chrome binary |
| `CHROME_NO_SANDBOX` | `false` (`true` in Docker) | Pass `--no-sandbox` to Chromium inside containers |
