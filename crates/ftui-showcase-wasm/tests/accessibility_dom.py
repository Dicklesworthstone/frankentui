#!/usr/bin/env python3
"""Run the shipped text bridge in Chromium without building WASM.

Requires Python Playwright and Chromium in the verification environment. No
package installation or external network is performed. Use --chromium to select
an executable. Native clipboard tests copy and paste using browser keyboard
defaults, without an origin or clipboard API. These are DOM/adapter tests, not
NVDA/VoiceOver speech tests.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
from pathlib import Path
import shutil

from playwright.sync_api import sync_playwright


SNAPSHOT = "textInput: Name. value Jos\u00e9 \u754c\U0001f980\ncheckbox: Agree. not checked"


def module_url(path: Path) -> str:
    return "data:text/javascript;base64," + base64.b64encode(path.read_bytes()).decode("ascii")


def native_review_cases(browser, api_url: str) -> list[dict]:
    results = []
    context = browser.new_context()
    # Fail closed on accidental network requests. All imports are data URLs.
    context.route("**/*", lambda route: route.abort())
    html = '''<!doctype html><html lang="en">
        <title>Offline terminal review test</title><body>
        <textarea id="outside-input" aria-label="Outside input"></textarea>
        <canvas id="terminal-canvas" tabindex="0" role="application"
          aria-label="Terminal" aria-describedby="existing-help"></canvas>
        <span id="existing-help" hidden>Existing terminal help</span>
        <div id="a11y-proxy" role="document" aria-live="polite"></div>
        <button id="after-review">After review</button></body></html>'''

    def fixture():
        page = context.new_page()
        page.set_default_timeout(10_000)
        page.set_content(html)
        page.evaluate('''async ({apiUrl, snapshot}) => {
            const api = await import(apiUrl);
            class RunnerContract {
                enabled = false;
                frame = 0;
                lines = snapshot.split('\\n');
                init() {}
                step() { this.frame += 1; return {rendered: true, running: true}; }
                setAccessibilityEnabled(enabled) { this.enabled = enabled; }
                takeAccessibilityUpdateJson() {
                    return JSON.stringify({schema_version: 1, enabled: this.enabled,
                        frame_id: String(this.frame), focus_id: '1', lines: this.lines,
                        omitted_nodes: 0, dropped_count: 0, announcements: []});
                }
                destroy() {}
                free() {}
            }
            window.runner = new (api.withShowcaseAccessibility(RunnerContract))();
            runner.init();
            window.hostEvents = [];
            // The real host installs its window handlers after runner init.
            // Catch regressions even if a handler cancels every forwarded key.
            for (const type of ['keydown', 'keyup', 'paste', 'copy', 'beforeinput', 'input']) {
                window.addEventListener(type, event => {
                    hostEvents.push({type, key: event.key || '', target: event.target.id});
                    if (event.target.id === 'terminal-canvas') event.preventDefault();
                }, true);
            }
        }''', {"apiUrl": api_url, "snapshot": SNAPSHOT})
        return page

    def active(page):
        return page.evaluate("document.activeElement.getAttribute('data-ftui-review-text') !== null")

    def open_review(page):
        page.locator("#terminal-canvas").focus()
        page.keyboard.press("Alt+Shift+r")
        assert active(page), "shortcut must focus the native snapshot textarea"
        page.evaluate("hostEvents.length = 0")

    def run(name, body):
        page = fixture()
        try:
            body(page)
            results.append({"name": name, "passed": True})
        except Exception as error:
            results.append({"name": name, "passed": False, "error": str(error)})
        finally:
            page.close()

    def copy_and_readonly(page):
        open_review(page)
        page.keyboard.press("Control+a")
        page.keyboard.press("Control+c")
        assert page.evaluate("hostEvents.length") == 0, "copy leaked to host listeners"
        page.locator("#outside-input").focus()
        page.keyboard.press("Control+v")
        copied = page.locator("#outside-input").input_value()
        assert copied == SNAPSHOT, "native clipboard must contain exactly the reviewed text"
        page.locator("[data-ftui-review-text]").focus()
        page.evaluate("hostEvents.length = 0")
        page.keyboard.type("must not edit or reach the terminal")
        assert page.locator("[data-ftui-review-text]").input_value() == SNAPSHOT
        assert page.evaluate("hostEvents.length") == 0, "review input leaked to host listeners"

    def escape_and_release(page):
        open_review(page)
        page.keyboard.press("Escape")
        assert page.evaluate("document.activeElement.id") == "terminal-canvas"
        assert page.locator("[data-ftui-review]").is_hidden()
        assert page.evaluate("hostEvents.length") == 0, "Escape keyup leaked after focus return"
        page.keyboard.press("x")
        assert page.evaluate("hostEvents.some(event => event.key === 'x')"), "terminal routing did not resume"

    def tab_no_trap(page):
        open_review(page)
        page.keyboard.press("Tab")
        assert page.evaluate("document.activeElement.textContent") == "Refresh snapshot"
        page.keyboard.press("Tab")
        assert page.evaluate("document.activeElement.textContent") == "Return to terminal"
        page.keyboard.press("Tab")
        assert page.evaluate("document.activeElement.id") == "after-review", "review trapped Tab"
        assert page.evaluate("hostEvents.length") == 0, "Tab release escaped its original owner"
        page.keyboard.press("Shift+Tab")
        assert page.evaluate("document.activeElement.textContent") == "Return to terminal"
        page.keyboard.press("Enter")
        assert page.evaluate("document.activeElement.id") == "terminal-canvas"
        assert page.locator("[data-ftui-review]").is_hidden()

    def button_activation(page):
        button = page.get_by_role("button", name="Read terminal", exact=True)
        button.focus()
        page.keyboard.press("Space")
        assert active(page), "native Space activation was canceled"
        page.keyboard.press("Escape")
        button.focus()
        page.keyboard.press("Enter")
        assert active(page), "native Enter activation was canceled"

    def explicit_refresh(page):
        open_review(page)
        page.keyboard.press("Home")
        page.keyboard.press("Shift+ArrowRight")
        page.keyboard.press("Shift+ArrowRight")
        before = page.evaluate("({start: document.activeElement.selectionStart, end: document.activeElement.selectionEnd})")
        page.evaluate("runner.lines = ['New snapshot']; runner.step()")
        assert page.locator("[data-ftui-review-text]").input_value() == SNAPSHOT
        assert page.evaluate("document.activeElement.selectionEnd") == before["end"]
        page.keyboard.press("Tab")
        page.keyboard.press("Enter")
        assert active(page)
        assert page.locator("[data-ftui-review-text]").input_value() == "New snapshot"
        assert page.evaluate("document.activeElement.selectionStart") == before["start"]
        assert page.evaluate("document.activeElement.selectionEnd") == before["end"]

    def exposed_accessibility(page):
        open_review(page)
        session = context.new_cdp_session(page)
        nodes = session.send("Accessibility.getFullAXTree")["nodes"]
        matches = [node for node in nodes if not node.get("ignored")
                   and node.get("role", {}).get("value") == "textbox"
                   and node.get("name", {}).get("value") == "Terminal content snapshot"]
        assert len(matches) == 1, "native snapshot missing or duplicated in browser accessibility tree"
        node = matches[0]
        properties = {item["name"]: item["value"].get("value") for item in node.get("properties", [])}
        assert properties.get("readonly") is True, "readonly state missing from browser accessibility API"
        assert properties.get("focused") is True
        assert node.get("value", {}).get("value") == SNAPSHOT
        session.detach()

    try:
        run("native keyboard selects and copies Unicode snapshot without leaking input", copy_and_readonly)
        run("native Escape restores terminal focus and contains the release", escape_and_release)
        run("native Tab and Shift+Tab leave the non-modal review without a keyboard trap", tab_no_trap)
        run("native Read terminal button works with Space and Enter", button_activation)
        run("native refresh preserves the selection after a background render", explicit_refresh)
        run("Chromium accessibility API exposes one focused readonly snapshot textbox", exposed_accessibility)
    finally:
        context.close()
    return results


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--chromium", default=shutil.which("chromium"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    paths = {"api": root / "accessibility.mjs", "tests": root / "tests" / "accessibility_dom.mjs"}
    sources = {name: module_url(path) for name, path in paths.items()}
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(
            executable_path=args.chromium, headless=True,
            args=["--no-sandbox", "--disable-dev-shm-usage"], timeout=20_000,
        )
        try:
            page = browser.new_page()
            page.set_default_timeout(10_000)
            page.set_content('<input id="focus-target" aria-label="Focus sentinel">')
            results = page.evaluate("""async sources => {
                const api = await import(sources.api);
                const {browserCases, browserReviewCases} = await import(sources.tests);
                return await Promise.race([
                    (async () => [...await browserCases(api), ...await browserReviewCases(api)])(),
                    new Promise((_, reject) => setTimeout(() => reject(new Error('DOM suite timeout')), 10000)),
                ]);
            }""", sources)
            page.close()
            results.extend(native_review_cases(browser, sources["api"]))
            print(json.dumps({
                "browser": browser.version,
                "source_sha256": {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                                  for path in [*paths.values(), Path(__file__)]},
                "cases": results,
            }, indent=2))
            return 0 if len(results) == 36 and all(case["passed"] for case in results) else 1
        finally:
            browser.close()


if __name__ == "__main__":
    raise SystemExit(main())
