"""
Run this ONCE on a machine with a screen to save your X login session.

    python login.py

A browser window opens. Log in to X manually (including 2FA), wait until you
see your home timeline, then come back to the terminal and press Enter.
The session is saved to STATE_FILE (default ./data/storage_state.json).
Copy that file to the server's data volume if you deploy elsewhere.
"""
import json
import os
from pathlib import Path

from playwright.sync_api import sync_playwright

STATE_FILE = Path(os.environ.get("STATE_FILE", "./data/storage_state.json"))


def main() -> None:
    STATE_FILE.parent.mkdir(parents=True, exist_ok=True)
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=False)
        ctx = browser.new_context(viewport={"width": 1280, "height": 900})
        page = ctx.new_page()
        page.goto("https://x.com/i/flow/login")
        input("Log in in the browser window, then press Enter here... ")
        ctx.storage_state(path=str(STATE_FILE))
        browser.close()

    state = json.loads(STATE_FILE.read_text())
    has_auth = any(c.get("name") == "auth_token" for c in state.get("cookies", []))
    try:
        os.chmod(STATE_FILE, 0o600)
    except OSError:
        pass
    if has_auth:
        print(f"OK: session saved to {STATE_FILE}")
        print("Treat this file like a password. Never commit it to git.")
    else:
        print("WARNING: no auth_token cookie found. Login probably did not finish.")


if __name__ == "__main__":
    main()
