#!/usr/bin/env python3
"""UI tests: drive the real window through WebDriver against the mock Square.

The app under test is a build with the `ui-test` feature, started with
DSF_MOCK=1 and a fresh DSF_DATA_DIR, so it talks to a mock Square on
loopback with a demo gift shop. The test walks the owner's path:
connect, set up, first sync, the report and its filters, save, the status
line and Check now, Settings, Help (Check this computer, diagnostics),
Remove all labels and Disconnect. Screenshots go to --shots.

Linux: tauri-driver (WebKitWebDriver). Windows: Edge WebDriver attached to
the app's WebView2 debugging port (--attach).

No third-party Python packages: a few lines of WebDriver over urllib.
"""

import argparse
import base64
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from pathlib import Path

ELEMENT = "element-6066-11e4-a52e-4f735466cecf"


class Driver:
    def __init__(self, url: str):
        self.url = url.rstrip("/")
        self.session = None

    def call(self, method: str, path: str, body=None, timeout=60):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(self.url + path, data=data, method=method, headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=timeout) as r:
                return json.loads(r.read() or b"{}").get("value")
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"{method} {path}: {e.code} {e.read()[:500]!r}") from None

    def s(self, method: str, path: str, body=None, timeout=60):
        return self.call(method, f"/session/{self.session}{path}", body, timeout)

    def start(self, caps: dict):
        value = self.call("POST", "/session", {"capabilities": {"alwaysMatch": caps}}, timeout=120)
        self.session = value["sessionId"]

    def quit(self):
        if self.session:
            try:
                self.call("DELETE", f"/session/{self.session}")
            except Exception:
                pass

    def find(self, css: str, timeout=20.0):
        end = time.time() + timeout
        last = None
        while time.time() < end:
            try:
                el = self.s("POST", "/element", {"using": "css selector", "value": css})
                return el[ELEMENT]
            except RuntimeError as e:
                last = e
                time.sleep(0.2)
        raise AssertionError(f"no element {css}: {last}")

    def finds(self, css: str):
        return [e[ELEMENT] for e in self.s("POST", "/elements", {"using": "css selector", "value": css})]

    def click(self, css: str, timeout=20.0):
        end = time.time() + timeout
        while True:
            el = self.find(css, timeout)
            try:
                self.s("POST", f"/element/{el}/click", {})
                return
            except RuntimeError:
                if time.time() > end:
                    raise
                time.sleep(0.3)

    def text(self, css: str, timeout=20.0) -> str:
        return self.s("GET", f"/element/{self.find(css, timeout)}/text")

    def js(self, script: str, *args):
        return self.s("POST", "/execute/sync", {"script": script, "args": list(args)})

    def js_async(self, script: str, *args):
        return self.s("POST", "/execute/async", {"script": script, "args": list(args)}, timeout=120)

    def wait_text(self, css: str, needle: str, timeout=60.0) -> str:
        end = time.time() + timeout
        seen = ""
        while time.time() < end:
            try:
                seen = self.js("const e = document.querySelector(arguments[0]); return e ? e.textContent : '';", css)
                if needle in seen:
                    return seen
            except RuntimeError:
                pass
            time.sleep(0.3)
        raise AssertionError(f"{css} never showed {needle!r}; last saw {seen!r}")

    def accept_alert(self, timeout=10.0):
        end = time.time() + timeout
        while time.time() < end:
            try:
                self.s("POST", "/alert/accept", {})
                return
            except RuntimeError:
                time.sleep(0.2)
        raise AssertionError("no confirmation dialog appeared")

    def shot(self, folder: Path, name: str):
        folder.mkdir(parents=True, exist_ok=True)
        png = self.s("GET", "/screenshot")
        (folder / f"{name}.png").write_bytes(base64.b64decode(png))


def free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_port(port: int, timeout=60.0):
    end = time.time() + timeout
    while time.time() < end:
        with socket.socket() as s:
            if s.connect_ex(("127.0.0.1", port)) == 0:
                return
        time.sleep(0.3)
    raise RuntimeError(f"nothing listening on {port}")


def run(d: Driver, shots: Path, data_dir: Path):
    results = []

    def step(name, fn):
        t = time.time()
        fn()
        d.shot(shots, f"{len(results) + 1:02d}-{name}")
        results.append((name, round(time.time() - t, 1)))
        print(f"ok  {name} ({results[-1][1]} s)", flush=True)

    def welcome():
        d.wait_text("main", "Connect Square")
        assert "Nothing leaves this computer except requests to Square" in d.text(".fine")

    def connect():
        d.click("#connect")
        d.wait_text("#connected-to", "Connected to Main St Boutique.", timeout=60)

    def setup():
        d.wait_text(".setup", "Labels in Square")
        assert d.js("return document.getElementById('set-labels').checked") is True
        assert d.js("return document.getElementById('set-background').checked") is True
        assert d.js("return document.getElementById('set-window').value") == "90"
        assert d.js("return document.getElementById('set-time').selectedOptions[0].textContent") == "5:00 a.m."
        d.click("#start")

    def first_sync():
        d.wait_text("#summary", "slow variation", timeout=120)
        rows = d.finds(".tr")
        assert len(rows) > 3, f"{len(rows)} rows"
        assert "SLOW" in d.text(".tr.slow .col-last_sold")

    def status_line():
        line = d.wait_text("#status-line", "labels on", timeout=30)
        assert "Connected to Main St Boutique" in line, line
        assert "last check today" in line, line

    def labels_in_square():
        d.wait_text(".tr .col-label", "In Square", timeout=30)

    def filters():
        d.click("#filter-slow")
        d.wait_text("#summary", "slow", timeout=20)
        all_rows = len(d.finds(".tr"))
        d.js("const s = document.getElementById('filter-search'); s.value = 'mug'; s.dispatchEvent(new Event('input'));")
        time.sleep(0.4)
        names = d.js("return [...document.querySelectorAll('.tr .col-item')].map(e => e.textContent)")
        assert names and all("mug" in n.lower() for n in names), names
        d.js("const s = document.getElementById('filter-search'); s.value = ''; s.dispatchEvent(new Event('input'));")
        d.js("const w = document.getElementById('filter-window'); w.value = '30'; w.dispatchEvent(new Event('change'));")
        d.wait_text("#summary", "slow", timeout=20)
        assert all_rows > 0
        d.click(".col-days")
        d.click(".col-days")

    def save():
        out = str(data_dir / "ui-export.csv")
        result = d.js_async(
            "const done = arguments[arguments.length - 1];"
            "window.__TAURI_INTERNALS__.invoke('export_to', {query: {window_days: 90, location: null, category: null, slow_only: true}, format: 'csv', path: arguments[0]}).then(done, e => done('ERR ' + e));",
            out,
        )
        assert result == out, result
        text = Path(out).read_text(encoding="utf-8-sig")
        assert text.startswith("Item,Variation,SKU"), text[:80]
        d.click("#save")
        d.find("#save-xlsx")

    def check_now():
        d.click("#check-now")
        d.wait_text("#check-now", "Check now", timeout=60)
        assert "labels on" in d.text("#status-line")

    def settings():
        d.click("#open-settings")
        d.find("#settings-save")
        d.js("document.getElementById('set-window').value = '60';")
        d.click("#settings-save")
        d.wait_text("#status-line", "labels on", timeout=60)
        d.wait_text("#check-now", "Check now", timeout=60)
        assert d.js("return document.getElementById('filter-window').value") == "60"

    def self_test():
        d.click("#help")
        d.click("#help-self-test")
        d.wait_text("#self-test-result", "All good", timeout=60)
        d.js("document.querySelector('dialog').close()")

    def diagnostics():
        d.click("#help")
        d.click("#help-diagnostics")
        text = d.wait_text("#diagnostics-text", "", timeout=20)
        text = d.js("return document.getElementById('diagnostics-text').value")
        assert "Idle Stock Finder diagnostics" in text
        for secret in ["Ceramic mug", "Main St Boutique", "EAAA"]:
            assert secret not in text, f"diagnostics show {secret}"
        d.js("document.querySelector('dialog').close()")

    def remove_labels():
        d.click("#open-settings")
        d.click("#remove-labels")
        d.accept_alert()
        d.wait_text("#settings-note", "Removed the Last sold label", timeout=60)
        d.js("document.querySelector('dialog').close()")
        d.wait_text("#status-line", "labels off", timeout=20)

    def disconnect():
        d.click("#open-settings")
        d.click("#disconnect")
        d.accept_alert()
        d.wait_text("main", "App integrations", timeout=20)
        d.find("#connect")

    step("welcome", welcome)
    step("connect", connect)
    step("setup", setup)
    step("first-sync", first_sync)
    step("status-line", status_line)
    step("labels-in-square", labels_in_square)
    step("filters", filters)
    step("save", save)
    step("check-now", check_now)
    step("settings", settings)
    step("self-test", self_test)
    step("diagnostics", diagnostics)
    step("remove-labels", remove_labels)
    step("disconnect", disconnect)
    return results


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--app", required=True)
    ap.add_argument("--shots", default="ui-shots")
    ap.add_argument("--attach", action="store_true", help="Windows: attach Edge WebDriver to the app's WebView2")
    ap.add_argument("--native-driver", help="msedgedriver.exe (with --attach) or WebKitWebDriver")
    args = ap.parse_args()
    shots = Path(args.shots)
    data_dir = Path(tempfile.mkdtemp(prefix="dsf-ui-"))
    env = dict(os.environ, DSF_MOCK="1", DSF_DATA_DIR=str(data_dir))
    procs = []
    d = None
    try:
        if args.attach:
            cdp = free_port()
            env["DSF_CDP_PORT"] = str(cdp)
            procs.append(subprocess.Popen([os.path.abspath(args.app)], env=env))
            wait_port(cdp, 90)
            port = free_port()
            procs.append(subprocess.Popen([args.native_driver, f"--port={port}"]))
            wait_port(port, 30)
            d = Driver(f"http://127.0.0.1:{port}")
            d.start({"browserName": "webview2", "ms:edgeOptions": {"debuggerAddress": f"127.0.0.1:{cdp}"}})
        else:
            port = free_port()
            driver = shutil.which("tauri-driver") or "tauri-driver"
            cmd = [driver, "--port", str(port)]
            if args.native_driver:
                cmd += ["--native-driver", args.native_driver]
            procs.append(subprocess.Popen(cmd, env=env))
            wait_port(port, 30)
            d = Driver(f"http://127.0.0.1:{port}")
            d.start({"tauri:options": {"application": os.path.abspath(args.app)}})
        results = run(d, shots, data_dir)
        (shots / "results.json").write_text(json.dumps(results, indent=2))
        print(f"UI tests: {len(results)} steps passed")
        return 0
    except Exception as e:
        print(f"UI tests FAILED: {e}", flush=True)
        if d and d.session:
            try:
                d.shot(shots, "failure")
                print(d.js("return document.body.innerText")[:3000])
            except Exception:
                pass
        return 1
    finally:
        if d:
            d.quit()
        for p in procs:
            p.terminate()
        shutil.rmtree(data_dir, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
