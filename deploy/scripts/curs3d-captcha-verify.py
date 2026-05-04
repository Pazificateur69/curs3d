#!/usr/bin/env python3
"""
Tiny stateless HTTP service that nginx calls via auth_request before letting a
faucet POST through. It validates the Cloudflare Turnstile token (or hCaptcha
token, configurable) the user submitted, and on success returns 200 with two
headers that the curs3d node trusts:

    X-Captcha-Verified: 1
    X-Captcha-Secret: <shared secret>

Required environment (via systemd EnvironmentFile=/etc/curs3d/captcha.env):

    CAPTCHA_PROVIDER       turnstile | hcaptcha
    CAPTCHA_SECRET_KEY     server-side secret from your Turnstile/hCaptcha dashboard
    CAPTCHA_SHARED_SECRET  must match CURS3D_FAUCET_CAPTCHA_SECRET on curs3d.service

Bind: 127.0.0.1:8090 (private; nginx proxies it)

How nginx wires it (snippet in deploy/nginx/captcha-snippet.conf).

Manual test:
    curl -X POST http://127.0.0.1:8090/verify -H 'X-Captcha-Token: <token>'
"""
import http.server
import json
import logging
import os
import sys
import urllib.parse
import urllib.request

PROVIDER = os.environ.get("CAPTCHA_PROVIDER", "turnstile").lower()
SECRET_KEY = os.environ.get("CAPTCHA_SECRET_KEY", "")
SHARED_SECRET = os.environ.get("CAPTCHA_SHARED_SECRET", "")
LISTEN_HOST = os.environ.get("CAPTCHA_LISTEN_HOST", "127.0.0.1")
LISTEN_PORT = int(os.environ.get("CAPTCHA_LISTEN_PORT", "8090"))

VERIFY_URL = {
    "turnstile": "https://challenges.cloudflare.com/turnstile/v0/siteverify",
    "hcaptcha":  "https://api.hcaptcha.com/siteverify",
}

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
log = logging.getLogger("captcha-verify")

if not SECRET_KEY or not SHARED_SECRET:
    log.error("CAPTCHA_SECRET_KEY and CAPTCHA_SHARED_SECRET are required")
    sys.exit(1)

if PROVIDER not in VERIFY_URL:
    log.error("unknown CAPTCHA_PROVIDER %s", PROVIDER)
    sys.exit(1)


def verify_token(token: str, remote_ip: str) -> bool:
    if not token:
        return False
    payload = {"secret": SECRET_KEY, "response": token}
    if remote_ip:
        payload["remoteip"] = remote_ip
    data = urllib.parse.urlencode(payload).encode()
    req = urllib.request.Request(VERIFY_URL[PROVIDER], data=data, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=8) as resp:
            body = json.loads(resp.read())
    except Exception as exc:
        log.warning("captcha verify backend error: %s", exc)
        return False
    ok = bool(body.get("success"))
    if not ok:
        log.info("captcha verify rejected: %s", body)
    return ok


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path != "/verify":
            self.send_response(404)
            self.end_headers()
            return
        token = (
            self.headers.get("X-Captcha-Token", "")
            or self.headers.get("Cf-Turnstile-Response", "")
            or self.headers.get("H-Captcha-Response", "")
        )
        ip = self.headers.get("X-Real-IP") or self.client_address[0]
        if verify_token(token.strip(), ip):
            self.send_response(204)
            self.send_header("X-Captcha-Verified", "1")
            self.send_header("X-Captcha-Secret", SHARED_SECRET)
            self.end_headers()
        else:
            self.send_response(403)
            self.end_headers()

    def log_message(self, fmt, *args):
        log.info("%s - %s", self.client_address[0], fmt % args)


if __name__ == "__main__":
    log.info("captcha verify listening on %s:%d (provider=%s)",
             LISTEN_HOST, LISTEN_PORT, PROVIDER)
    http.server.HTTPServer((LISTEN_HOST, LISTEN_PORT), Handler).serve_forever()
