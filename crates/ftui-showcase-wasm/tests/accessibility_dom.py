#!/usr/bin/env python3
"""Run the shipped text bridge in a real Chromium DOM, without building WASM.

Requires Python Playwright and Chromium in the verification environment. No
package installation or network is performed. Use --chromium to select a
configured executable. This is not an NVDA/VoiceOver speech test.
"""
from __future__ import annotations

import argparse
import base64
import json
from pathlib import Path
import shutil

from playwright.sync_api import sync_playwright


def module_url(path: Path) -> str:
    return "data:text/javascript;base64," + base64.b64encode(path.read_bytes()).decode("ascii")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--chromium", default=shutil.which("chromium"))
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    sources = {
        "api": module_url(root / "accessibility.mjs"),
        "tests": module_url(root / "tests" / "accessibility_dom.mjs"),
    }
    with sync_playwright() as playwright:
        browser = playwright.chromium.launch(
            executable_path=args.chromium,
            headless=True,
            args=["--no-sandbox", "--disable-dev-shm-usage"],
            timeout=20_000,
        )
        try:
            page = browser.new_page()
            page.set_default_timeout(10_000)
            page.set_content('<input id="focus-target" aria-label="Focus sentinel">')
            results = page.evaluate("""async sources => {
                const api = await import(sources.api);
                const {browserCases} = await import(sources.tests);
                return await Promise.race([
                    browserCases(api),
                    new Promise((_, reject) => setTimeout(() => reject(new Error('DOM suite timeout')), 10000)),
                ]);
            }""", sources)
            print(json.dumps({"browser": browser.version, "cases": results}, indent=2))
            return 0 if len(results) == 18 and all(case["passed"] for case in results) else 1
        finally:
            browser.close()


if __name__ == "__main__":
    raise SystemExit(main())
