#!/usr/bin/env python3
"""
Portable, Changeable Browser Skill for Luna Harness.
Supports:
  - 'search': Web search (DuckDuckGo HTML) with zero external dependencies.
  - 'fetch': Fast HTTP web page text and markdown extraction with SSRF security filter.
  - 'open': Headless Chromium browser via Playwright (if installed) with full JS execution.
  - 'eval': Evaluate JavaScript on a web page.
  - 'screenshot': Capture webpage screenshot.
"""

import sys
import os
import re
import json
import socket
import ipaddress
import urllib.request
import urllib.parse
from html.parser import HTMLParser

# ==============================================================================
# Changeable Configuration
# ==============================================================================
DEFAULT_USER_AGENT = (
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/124.0.0.0 Safari/537.36"
)
DEFAULT_TIMEOUT_SECS = 25
MAX_OUTPUT_CHARS = 30000

# ==============================================================================
# SSRF Protection
# ==============================================================================
PRIVATE_NETWORKS = [
    ipaddress.ip_network("127.0.0.0/8"),
    ipaddress.ip_network("10.0.0.0/8"),
    ipaddress.ip_network("172.16.0.0/12"),
    ipaddress.ip_network("192.168.0.0/16"),
    ipaddress.ip_network("169.254.0.0/16"),
    ipaddress.ip_network("0.0.0.0/8"),
    ipaddress.ip_network("::1/128"),
    ipaddress.ip_network("fc00::/7"),
    ipaddress.ip_network("fe80::/10"),
]

def assert_safe_url(url: str):
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme not in ("http", "https"):
        raise ValueError(f"Disallowed scheme: '{parsed.scheme}'. Only http and https URLs are permitted.")

    hostname = parsed.hostname or ""
    if not hostname:
        raise ValueError("URL must contain a valid hostname.")

    if hostname.lower() in ("localhost", "local", "ip6-localhost", "ip6-loopback"):
        raise ValueError(f"SSRF Refusal: Access to localhost '{hostname}' is blocked.")

    try:
        addr_info = socket.getaddrinfo(hostname, None)
        for _, _, _, _, sockaddr in addr_info:
            ip_str = sockaddr[0]
            ip_obj = ipaddress.ip_address(ip_str)
            for net in PRIVATE_NETWORKS:
                if ip_obj in net:
                    raise ValueError(f"SSRF Refusal: Access to private network address '{ip_str}' is blocked.")
    except socket.gaierror:
        # Let standard request fail on resolution if not resolved
        pass

# ==============================================================================
# HTML to Markdown Text Parser (Stdlib, Zero Pip Dependencies)
# ==============================================================================
class SimpleMarkdownExtractor(HTMLParser):
    def __init__(self):
        super().__init__()
        self.text_chunks = []
        self.in_ignored_tag = 0
        self.ignored_tags = {"script", "style", "svg", "noscript", "nav", "footer", "header", "aside"}
        self.current_link = None

    def handle_starttag(self, tag, attrs):
        tag = tag.lower()
        if tag in self.ignored_tags:
            self.in_ignored_tag += 1
            return
        if self.in_ignored_tag > 0:
            return

        attrs_dict = dict(attrs)
        if tag in ("h1", "h2", "h3", "h4", "h5", "h6"):
            level = int(tag[1])
            self.text_chunks.append(f"\n\n{'#' * level} ")
        elif tag in ("p", "div", "article", "section"):
            self.text_chunks.append("\n\n")
        elif tag == "li":
            self.text_chunks.append("\n- ")
        elif tag in ("pre", "code"):
            self.text_chunks.append(" `")
        elif tag == "blockquote":
            self.text_chunks.append("\n> ")
        elif tag == "a":
            href = attrs_dict.get("href", "")
            if href and not href.startswith("javascript:"):
                self.current_link = href
                self.text_chunks.append("[")

    def handle_endtag(self, tag):
        tag = tag.lower()
        if tag in self.ignored_tags:
            self.in_ignored_tag = max(0, self.in_ignored_tag - 1)
            return
        if self.in_ignored_tag > 0:
            return

        if tag in ("pre", "code"):
            self.text_chunks.append("` ")
        elif tag == "a" and self.current_link:
            self.text_chunks.append(f"]({self.current_link})")
            self.current_link = None
        elif tag in ("h1", "h2", "h3", "h4", "h5", "h6", "p", "div"):
            self.text_chunks.append("\n")

    def handle_data(self, data):
        if self.in_ignored_tag == 0:
            cleaned = re.sub(r"[ \t]+", " ", data)
            if cleaned:
                self.text_chunks.append(cleaned)

    def get_text(self) -> str:
        raw = "".join(self.text_chunks)
        # Collapse excessive newlines
        cleaned = re.sub(r"\n{3,}", "\n\n", raw).strip()
        return cleaned

# ==============================================================================
# DuckDuckGo HTML Search (Stdlib, Zero Pip Dependencies)
# ==============================================================================
class DuckDuckGoParser(HTMLParser):
    def __init__(self):
        super().__init__()
        self.results = []
        self.cur_link = None
        self.cur_title = []
        self.cur_snippet = []
        self.in_title = False
        self.in_snippet = False

    def handle_starttag(self, tag, attrs):
        attrs_dict = dict(attrs)
        cls = attrs_dict.get("class", "")
        if tag == "a" and "result__a" in cls:
            href = attrs_dict.get("href", "")
            if "uddg=" in href:
                parsed = urllib.parse.parse_qs(urllib.parse.urlparse(href).query)
                href = parsed.get("uddg", [href])[0]
            self.cur_link = href
            self.in_title = True
            self.cur_title = []
        elif "result__snippet" in cls:
            self.in_snippet = True
            self.cur_snippet = []

    def handle_endtag(self, tag):
        if tag == "a" and self.in_title:
            self.in_title = False
        elif self.in_snippet and tag in ("a", "div", "td", "p"):
            self.in_snippet = False
            if self.cur_link:
                title = "".join(self.cur_title).strip()
                snippet = "".join(self.cur_snippet).strip()
                if title:
                    self.results.append({"title": title, "url": self.cur_link, "snippet": snippet})
                self.cur_link = None

    def handle_data(self, data):
        if self.in_title:
            self.cur_title.append(data)
        elif self.in_snippet:
            self.cur_snippet.append(data)

def do_search(query: str, limit: int = 5) -> str:
    if not query.strip():
        return "Search error: query cannot be empty"

    url = "https://html.duckduckgo.com/html/?q=" + urllib.parse.quote(query)
    req = urllib.request.Request(
        url,
        headers={"User-Agent": DEFAULT_USER_AGENT, "Accept-Language": "en-US,en;q=0.9"}
    )
    try:
        with urllib.request.urlopen(req, timeout=DEFAULT_TIMEOUT_SECS) as resp:
            html = resp.read().decode("utf-8", errors="ignore")
    except Exception as e:
        return f"Search network error: {e}"

    parser = DuckDuckGoParser()
    parser.feed(html)

    if not parser.results:
        return f"No results found for '{query}'."

    out = [f"Search results for **{query}**:\n"]
    for i, r in enumerate(parser.results[:limit], 1):
        out.append(f"{i}. [{r['title']}]({r['url']})\n   {r['snippet']}\n")
    return "\n".join(out).strip()

def do_fetch(url: str) -> str:
    assert_safe_url(url)
    req = urllib.request.Request(
        url,
        headers={"User-Agent": DEFAULT_USER_AGENT, "Accept": "text/html,application/xhtml+xml,text/plain"}
    )
    try:
        with urllib.request.urlopen(req, timeout=DEFAULT_TIMEOUT_SECS) as resp:
            content_type = resp.headers.get("Content-Type", "")
            raw_bytes = resp.read(MAX_OUTPUT_CHARS * 2)
            charset = resp.headers.get_content_charset() or "utf-8"
            html = raw_bytes.decode(charset, errors="ignore")
    except Exception as e:
        return f"Fetch error ({url}): {e}"

    parser = SimpleMarkdownExtractor()
    parser.feed(html)
    text = parser.get_text()

    if not text:
        text = re.sub(r"<[^>]+>", " ", html)
        text = re.sub(r"\s+", " ", text).strip()

    if len(text) > MAX_OUTPUT_CHARS:
        text = text[:MAX_OUTPUT_CHARS] + f"\n\n… [Truncated at {MAX_OUTPUT_CHARS} characters]"

    return f"URL: {url}\n\n{text}"

# ==============================================================================
# Playwright Headless Browser Automation (Async/Sync)
# ==============================================================================
def do_open_browser(url: str, js_code: str | None = None, screenshot_path: str | None = None) -> str:
    assert_safe_url(url)
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        # Fallback to standard fetch if playwright is not installed
        res = do_fetch(url)
        return f"[Notice: Playwright not installed. Used fast HTTP fetch instead]\n\n{res}"

    try:
        with sync_playwright() as p:
            browser = p.chromium.launch(headless=True)
            page = browser.new_page(user_agent=DEFAULT_USER_AGENT)
            page.set_default_timeout(DEFAULT_TIMEOUT_SECS * 1000)

            page.goto(url, wait_until="domcontentloaded")
            title = page.title()

            eval_result = None
            if js_code:
                try:
                    eval_result = page.evaluate(js_code)
                except Exception as e:
                    eval_result = f"JS eval error: {e}"

            if screenshot_path:
                os.makedirs(os.path.dirname(os.path.abspath(screenshot_path)), exist_ok=True)
                page.screenshot(path=screenshot_path, full_page=False)

            content = page.evaluate('''() => {
                const clone = document.body.cloneNode(true);
                ['script', 'style', 'nav', 'footer', 'noscript', 'svg'].forEach(tag => {
                    clone.querySelectorAll(tag).forEach(el => el.remove());
                });
                return clone.innerText;
            }''')

            browser.close()

            text = re.sub(r"\n{3,}", "\n\n", (content or "").strip())
            if len(text) > MAX_OUTPUT_CHARS:
                text = text[:MAX_OUTPUT_CHARS] + f"\n\n… [Truncated at {MAX_OUTPUT_CHARS} characters]"

            report = [f"Title: {title}", f"URL: {url}"]
            if eval_result is not None:
                report.append(f"JS Eval Result: {eval_result}")
            if screenshot_path:
                report.append(f"Screenshot saved to: {os.path.abspath(screenshot_path)}")
            report.append(f"\n{text}")

            return "\n".join(report)
    except Exception as e:
        # If chromium fails to launch, fall back to HTTP fetch
        res = do_fetch(url)
        return f"[Notice: Browser execution failed ({e}). Fallback to HTTP fetch:]\n\n{res}"

# ==============================================================================
# Entry Point & CLI Dispatch
# ==============================================================================
def main():
    args_dict = {}

    # 1. Read from stdin if piped
    if not sys.stdin.isatty():
        try:
            stdin_text = sys.stdin.read().strip()
            if stdin_text.startswith("{") and stdin_text.endswith("}"):
                args_dict = json.loads(stdin_text)
            elif stdin_text:
                args_dict = {"input": stdin_text}
        except Exception:
            pass

    # 2. Merge with SKILL_ARGS env if present
    env_args = os.environ.get("SKILL_ARGS", "")
    if env_args.strip().startswith("{"):
        try:
            args_dict.update(json.loads(env_args))
        except Exception:
            pass

    # 3. CLI args fallback
    if len(sys.argv) > 1:
        first = sys.argv[1].lower()
        if first in ("search", "find"):
            args_dict["action"] = "search"
            args_dict["query"] = " ".join(sys.argv[2:])
        elif first in ("fetch", "get", "read"):
            args_dict["action"] = "fetch"
            args_dict["url"] = sys.argv[2] if len(sys.argv) > 2 else ""
        elif first in ("open", "browse"):
            args_dict["action"] = "open"
            args_dict["url"] = sys.argv[2] if len(sys.argv) > 2 else ""
        elif first in ("screenshot", "shot"):
            args_dict["action"] = "screenshot"
            args_dict["url"] = sys.argv[2] if len(sys.argv) > 2 else ""
            args_dict["screenshot_path"] = sys.argv[3] if len(sys.argv) > 3 else "/tmp/screenshot.png"
        elif first.startswith("http://") or first.startswith("https://"):
            args_dict["action"] = "fetch"
            args_dict["url"] = sys.argv[1]
        else:
            if "action" not in args_dict:
                args_dict["action"] = "search"
                args_dict["query"] = " ".join(sys.argv[1:])

    # Resolve action and target
    action = args_dict.get("action", "").lower().strip()
    query = args_dict.get("query", "") or args_dict.get("input", "")
    url = args_dict.get("url", "") or (query if query.startswith("http://") or query.startswith("https://") else "")
    limit = int(args_dict.get("limit", 5))

    # Auto-infer action if missing
    if not action:
        if url.startswith("http://") or url.startswith("https://"):
            action = "fetch"
        else:
            action = "search"

    try:
        if action in ("search", "find", "google", "ddg"):
            target_query = query if not url else query or url
            print(do_search(target_query, limit=limit))
        elif action in ("fetch", "read", "extract", "get"):
            if not url:
                print("Error: 'url' parameter required for fetch action.", file=sys.stderr)
                sys.exit(1)
            print(do_fetch(url))
        elif action in ("open", "browse", "render"):
            if not url:
                print("Error: 'url' parameter required for open action.", file=sys.stderr)
                sys.exit(1)
            js_code = args_dict.get("js_code")
            screenshot = args_dict.get("screenshot_path")
            print(do_open_browser(url, js_code=js_code, screenshot_path=screenshot))
        elif action == "screenshot":
            if not url:
                print("Error: 'url' parameter required for screenshot action.", file=sys.stderr)
                sys.exit(1)
            screenshot = args_dict.get("screenshot_path", "/tmp/luna_screenshot.png")
            print(do_open_browser(url, screenshot_path=screenshot))
        elif action in ("eval", "js"):
            if not url:
                print("Error: 'url' parameter required for eval action.", file=sys.stderr)
                sys.exit(1)
            js_code = args_dict.get("js_code") or query
            print(do_open_browser(url, js_code=js_code))
        else:
            print(f"Unknown browser action '{action}'. Available: search, fetch, open, eval, screenshot.", file=sys.stderr)
            sys.exit(1)
    except ValueError as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)

if __name__ == "__main__":
    main()
