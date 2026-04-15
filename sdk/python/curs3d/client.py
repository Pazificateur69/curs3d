"""CURS3D blockchain client for Python."""

from __future__ import annotations

import asyncio
import json
import threading
from typing import Any, Callable

import requests

try:
    import websockets
    import websockets.client
    HAS_WEBSOCKETS = True
except ImportError:
    HAS_WEBSOCKETS = False


class CursError(Exception):
    """Error returned by the CURS3D API."""
    pass


class CursClient:
    """Client for the CURS3D quantum-resistant blockchain.

    Usage:
        client = CursClient("http://localhost:8080")
        status = client.get_status()
        account = client.get_account("CUR...")
    """

    def __init__(self, node_url: str, timeout: float = 30.0):
        self.base_url = node_url.rstrip("/")
        self.timeout = timeout
        self._ws_thread: threading.Thread | None = None
        self._ws_stop = threading.Event()

    def _request(self, method: str, path: str, **kwargs: Any) -> Any:
        url = f"{self.base_url}{path}"
        resp = requests.request(method, url, timeout=self.timeout, **kwargs)
        data = resp.json()
        if not data.get("ok"):
            raise CursError(data.get("error", "Unknown API error"))
        return data.get("data")

    # ─── Chain Queries ────────────────────────────────────────────

    def get_status(self) -> dict:
        return self._request("GET", "/api/status")

    def get_health(self) -> dict:
        return self._request("GET", "/api/healthz")

    def get_block(self, height: int) -> dict:
        return self._request("GET", f"/api/block/{height}")

    def get_blocks(self, from_height: int | None = None, limit: int | None = None) -> list[dict]:
        params = {}
        if from_height is not None:
            params["from"] = from_height
        if limit is not None:
            params["limit"] = limit
        return self._request("GET", "/api/blocks", params=params)

    def get_account(self, address: str) -> dict:
        return self._request("GET", f"/api/account/{address}")

    def get_transaction(self, tx_hash: str) -> dict:
        return self._request("GET", f"/api/tx/{tx_hash}")

    def get_receipt(self, tx_hash: str) -> dict:
        return self._request("GET", f"/api/receipt/{tx_hash}")

    def get_validators(self) -> list[dict]:
        return self._request("GET", "/api/validators")

    def get_pending(self) -> list[dict]:
        return self._request("GET", "/api/pending")

    def request_faucet(self, address: str) -> dict:
        return self._request("POST", "/api/faucet/request", json={"address": address})

    def submit_transaction(self, signed_tx: dict) -> dict:
        return self._request("POST", "/api/tx/submit", json=signed_tx)

    def estimate_gas(self, tx: dict) -> dict:
        return self._request("POST", "/api/tx/estimate", json=tx)

    # ─── CUR-20 Tokens ───────────────────────────────────────────

    def get_tokens(self) -> list[dict]:
        return self._request("GET", "/api/tokens")

    def get_token(self, token_address: str) -> dict:
        return self._request("GET", f"/api/token/{token_address}")

    def get_token_balance(self, token_address: str, owner_address: str) -> dict:
        return self._request("GET", f"/api/token/{token_address}/balance/{owner_address}")

    # ─── Governance ──────────────────────────────────────────────

    def get_proposals(self) -> list[dict]:
        return self._request("GET", "/api/governance/proposals")

    def get_proposal(self, proposal_id: str) -> dict:
        return self._request("GET", f"/api/governance/proposal/{proposal_id}")

    # ─── WebSocket ───────────────────────────────────────────────

    def subscribe(
        self,
        events: list[str],
        callback: Callable[[dict], None],
    ) -> None:
        """Subscribe to real-time events via WebSocket.

        Runs in a background thread. Call `disconnect()` to stop.

        Args:
            events: List of event types to subscribe to
                    (e.g., ["new_block", "new_transaction", "finality"])
            callback: Function called with each event dict
        """
        if not HAS_WEBSOCKETS:
            raise ImportError("Install 'websockets' package for WebSocket support")

        ws_url = self.base_url.replace("http://", "ws://").replace("https://", "wss://") + "/ws"
        self._ws_stop.clear()

        async def _ws_loop():
            async with websockets.client.connect(ws_url) as ws:
                await ws.send(json.dumps({"events": events}))
                while not self._ws_stop.is_set():
                    try:
                        msg = await asyncio.wait_for(ws.recv(), timeout=1.0)
                        event = json.loads(msg)
                        callback(event)
                    except asyncio.TimeoutError:
                        continue
                    except Exception:
                        break

        def _run():
            asyncio.run(_ws_loop())

        self._ws_thread = threading.Thread(target=_run, daemon=True)
        self._ws_thread.start()

    def disconnect(self) -> None:
        """Stop WebSocket subscription."""
        self._ws_stop.set()
        if self._ws_thread:
            self._ws_thread.join(timeout=5.0)
            self._ws_thread = None
