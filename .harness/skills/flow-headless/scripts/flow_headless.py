#!/usr/bin/env python3
"""Headless Google Flow driver.

Drives Flow and YouTube Studio in headless Firefox using a trimmed copy of the
user's real Firefox profile, so the Google session comes along without a login.

Every action prints one JSON object on stdout. Diagnostics go to stderr.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

SKILL_DIR = Path(__file__).resolve().parent.parent
PROFILE_DIR = SKILL_DIR / "profile"
DEFAULT_DOWNLOADS = SKILL_DIR / "downloads"
DEFAULT_PROJECT = "https://flow.google.com/project/7815a713-558e-41d7-b992-a063d04174eb"
AUTH_FILES = ("prefs.js", "key4.db", "logins.json", "cert9.db", "pkcs11.txt", "permissions.sqlite")
DOWNLOAD_MIME = "video/mp4,application/octet-stream,application/mp4"


def emit(**fields):
    print(json.dumps(fields, ensure_ascii=False))


def log(*args):
    print(*args, file=sys.stderr, flush=True)


# --------------------------------------------------------------------------
# Dependencies
# --------------------------------------------------------------------------

def ensure_selenium() -> None:
    """Re-exec inside a local venv when Selenium is not importable."""
    try:
        import selenium  # noqa: F401
        return
    except ImportError:
        pass
    venv = SKILL_DIR / ".venv"
    python = venv / "bin" / "python"
    if not python.exists():
        log("creating venv and installing selenium...")
        subprocess.run([sys.executable, "-m", "venv", str(venv)], check=True)
        subprocess.run([str(venv / "bin" / "pip"), "install", "-q", "selenium"], check=True)
    os.execv(str(python), [str(python), str(Path(__file__).resolve()), *sys.argv[1:]])


# --------------------------------------------------------------------------
# Profile
# --------------------------------------------------------------------------

def newest_firefox_profile() -> Path:
    base = Path.home() / ".config" / "mozilla" / "firefox"
    candidates = sorted(
        base.glob("*/cookies.sqlite"), key=lambda p: p.stat().st_mtime, reverse=True
    )
    if not candidates:
        raise SystemExit(f"no Firefox profile with cookies found under {base}")
    return candidates[0].parent


def sync_profile() -> dict:
    """Copy the auth-bearing files from the live Firefox profile."""
    src = newest_firefox_profile()
    PROFILE_DIR.mkdir(parents=True, exist_ok=True)
    copied = []
    for name in AUTH_FILES:
        source = src / name
        if source.exists():
            shutil.copy2(source, PROFILE_DIR / name)
            copied.append(name)

    cookies_src = src / "cookies.sqlite"
    cookies_dst = PROFILE_DIR / "cookies.sqlite"
    if cookies_dst.exists():
        cookies_dst.unlink()
    source = sqlite3.connect(f"file:{cookies_src}?mode=ro", uri=True)
    target = sqlite3.connect(cookies_dst)
    try:
        source.backup(target)  # consistent snapshot even while Firefox is running
    finally:
        target.close()
        source.close()
    copied.append("cookies.sqlite")

    count = sqlite3.connect(cookies_dst).execute("select count(*) from moz_cookies").fetchone()[0]
    return {"profile": str(src), "files": copied, "cookies": count}


# --------------------------------------------------------------------------
# Driver
# --------------------------------------------------------------------------

def make_driver(downloads: Path):
    from selenium import webdriver
    from selenium.webdriver.firefox.options import Options

    if not (PROFILE_DIR / "cookies.sqlite").exists():
        log("no headless profile yet; copying from Firefox")
        sync_profile()

    downloads.mkdir(parents=True, exist_ok=True)
    opts = Options()
    opts.add_argument("-headless")
    opts.add_argument("--no-remote")  # never hand off to the visible Firefox
    opts.add_argument("--width=1600")
    opts.add_argument("--height=1000")
    opts.profile = str(PROFILE_DIR)
    opts.set_preference("browser.download.folderList", 2)
    opts.set_preference("browser.download.dir", str(downloads))
    opts.set_preference("browser.download.useDownloadDir", True)
    opts.set_preference("browser.helperApps.neverAsk.saveToDisk", DOWNLOAD_MIME)
    opts.set_preference("browser.download.manager.showWhenStarting", False)
    opts.set_preference("browser.sessionstore.resume_from_crash", False)
    opts.set_preference("dom.webdriver.enabled", False)
    opts.set_preference("media.autoplay.default", 5)

    driver = webdriver.Firefox(options=opts)
    driver.set_page_load_timeout(120)
    return driver


# --------------------------------------------------------------------------
# Flow helpers
# --------------------------------------------------------------------------

PROMPT_BOX = "[role='textbox'][aria-label='What do you want to create?']"


def body(driver) -> str:
    return driver.execute_script("return document.body.innerText || ''") or ""


def visible(driver, css: str):
    from selenium.webdriver.common.by import By

    return [e for e in driver.find_elements(By.CSS_SELECTOR, css) if e.is_displayed()]


def by_aria(driver, name: str):
    from selenium.webdriver.common.by import By

    for el in driver.find_elements(By.CSS_SELECTOR, "[aria-label]"):
        if (el.get_attribute("aria-label") or "").strip().lower() == name.lower() and el.is_displayed():
            return el
    return None


def click_radio(driver, text: str):
    """Click a Material button-toggle radio whose label contains `text`."""
    from selenium.webdriver.common.by import By

    for el in driver.find_elements(By.CSS_SELECTOR, "button[role='radio']"):
        if el.is_displayed() and text.lower() in (el.text or "").lower():
            el.click()
            return (el.text or "").replace("\n", " / ")
    return None


def settings_chip(driver) -> str:
    for el in driver.find_elements("css selector", "[aria-label='Settings trigger']"):
        return (el.text or "").replace("\n", " / ")
    return ""


def video_thumbnail_count(driver) -> int:
    from selenium.webdriver.common.by import By

    return len(driver.find_elements(By.CSS_SELECTOR, "img[alt='Generated video thumbnail']"))


def open_project(driver, project: str):
    driver.get(project)
    from selenium.webdriver.support.ui import WebDriverWait

    WebDriverWait(driver, 90).until(
        lambda d: d.execute_script("return document.readyState") == "complete"
    )
    time.sleep(6)


def set_video_mode(driver, count: int, length: int | None):
    """Open the settings panel, switch to Video, choose the output count."""
    trigger = by_aria(driver, "Settings trigger")
    if trigger is None:
        raise SystemExit("settings trigger not found")
    trigger.click()
    time.sleep(2)

    label = click_radio(driver, "Video")
    if label is None:
        raise SystemExit("video mode radio not found")
    time.sleep(3)

    if count and count != 1:
        if click_radio(driver, f"x{count}") is None:
            raise SystemExit(f"output count x{count} not found")
        time.sleep(2)

    chip = settings_chip(driver)
    if "video" not in chip.lower():
        raise SystemExit(f"refusing to generate: mode chip reads {chip!r}")

    from selenium.webdriver.common.keys import Keys

    driver.find_element("tag name", "body").send_keys(Keys.ESCAPE)
    time.sleep(2)
    return chip


def submit_prompt(driver, prompt: str):
    from selenium.webdriver.common.by import By

    box = driver.find_element(By.CSS_SELECTOR, PROMPT_BOX)
    box.click()
    box.send_keys(prompt)
    time.sleep(2)

    chip = settings_chip(driver)
    if "video" not in chip.lower():
        raise SystemExit(f"refusing to generate: mode changed to {chip!r}")

    gen = by_aria(driver, "Start generation")
    if gen is None:
        raise SystemExit("Start generation button not found")
    gen.click()
    return chip


def wait_for_clip(driver, before: int, timeout: int) -> int:
    started = time.time()
    while time.time() - started < timeout:
        time.sleep(8)
        now = video_thumbnail_count(driver)
        log(f"[{int(time.time() - started):>4}s] thumbnails={now} (before {before})")
        if now > before:
            return now
    raise SystemExit(f"no new clip after {timeout}s")


def open_newest_tile(driver) -> str:
    """Open the first grid tile with a real pointer click and return the scene URL."""
    from selenium.webdriver import ActionChains
    from selenium.webdriver.common.by import By

    tiles = visible(driver, "flow-grid-tile-container")
    if not tiles:
        raise SystemExit("no grid tiles to open")
    tile = tiles[0]
    driver.execute_script("arguments[0].scrollIntoView({block:'center'})", tile)
    time.sleep(1)
    ActionChains(driver).move_to_element(tile).pause(1).click().perform()
    time.sleep(8)
    return driver.current_url


def find_download_control(driver, timeout: int = 40):
    from selenium.webdriver.common.by import By

    started = time.time()
    while time.time() - started < timeout:
        for el in driver.find_elements(By.CSS_SELECTOR, "button, [role='button'], a"):
            if not el.is_displayed():
                continue
            label = ((el.text or "") + " " + (el.get_attribute("aria-label") or "")).lower()
            if "download" in label:
                return el
        time.sleep(2)
    return None


def click_download(driver, quality: str):
    control = find_download_control(driver)
    if control is None:
        raise SystemExit("download control not found in the editor")
    control.click()
    time.sleep(3)

    wanted = {"720p": "720p", "1080p": "1080p", "4k": "4k", "gif": "gif"}.get(quality.lower(), "720p")
    items = visible(driver, "[role='menuitem'], .mat-mdc-menu-item")
    pick = None
    for el in items:
        if wanted in (el.text or "").lower():
            pick = el
            break
    if pick is None:
        labels = [(e.text or "").strip() for e in items]
        raise SystemExit(f"quality {quality!r} not offered; menu was {labels}")
    label = (pick.text or "").replace("\n", " / ")
    pick.click()  # the menu closes, so read the label first
    return label


# --------------------------------------------------------------------------
# Actions
# --------------------------------------------------------------------------

def action_status(args) -> dict:
    driver = make_driver(Path(args.out) if args.out else DEFAULT_DOWNLOADS)
    try:
        open_project(driver, args.project)
        account = ""
        for el in driver.find_elements("css selector", "[aria-label]"):
            label = el.get_attribute("aria-label") or ""
            if label.startswith("Google Account:"):
                account = label.replace("\n", " ").strip()
                break
        tiles = len(visible(driver, "flow-grid-tile-container"))

        driver.get(args.project.rstrip("/") + "/trash")
        time.sleep(6)
        trash = next(
            (line for line in body(driver).splitlines() if "in trash" in line), "unknown"
        )
        return {"ok": True, "account": account, "tiles": tiles, "trash": trash}
    finally:
        driver.quit()


def action_generate(args) -> dict:
    prompt = args.prompt
    if args.prompt_file:
        prompt = Path(args.prompt_file).read_text().strip()
    if not prompt:
        raise SystemExit("generate needs --prompt or --prompt-file")

    downloads = Path(args.out) if args.out else DEFAULT_DOWNLOADS
    driver = make_driver(downloads)
    try:
        open_project(driver, args.project)
        chip = set_video_mode(driver, args.count, args.length)
        before = video_thumbnail_count(driver)
        submitted = submit_prompt(driver, prompt)
        log(f"submitted in {submitted!r}; waiting for a clip")
        wait_for_clip(driver, before, args.timeout_secs)
        scene = open_newest_tile(driver)
        return {"ok": True, "scene": scene, "chip": chip, "prompt": prompt}
    finally:
        driver.quit()


def action_download(args) -> dict:
    if not args.scene:
        raise SystemExit("download needs --scene")
    out = Path(args.out) if args.out else DEFAULT_DOWNLOADS
    out.mkdir(parents=True, exist_ok=True)
    before = set(os.listdir(out))

    driver = make_driver(out)
    try:
        driver.get(args.scene)
        time.sleep(8)
        if find_download_control(driver, timeout=25) is None:
            # A direct scene URL can be a transient copy after a restore. Resolve
            # the live scene by opening the newest tile from the project instead.
            log("download control not on the scene URL; resolving from the project")
            open_project(driver, args.project)
            from selenium.webdriver import ActionChains

            tiles = visible(driver, "flow-grid-tile-container")
            if not tiles:
                raise SystemExit("the project has no clips to download")
            ActionChains(driver).move_to_element(tiles[0]).pause(1).click().perform()
            time.sleep(8)

        picked = click_download(driver, args.quality)

        started = time.time()
        while time.time() - started < 180:
            time.sleep(3)
            new = [f for f in set(os.listdir(out)) - before if not f.endswith(".part")]
            if new:
                path = out / new[0]
                return {
                    "ok": True,
                    "file": str(path),
                    "bytes": path.stat().st_size,
                    "quality": picked,
                }
        raise SystemExit("download did not produce a file within 180s")
    finally:
        driver.quit()


DEEP_JS = """
function deepAll(root, sel, out) {
  for (const e of root.querySelectorAll(sel)) out.push(e);
  for (const e of root.querySelectorAll('*')) if (e.shadowRoot) deepAll(e.shadowRoot, sel, out);
  return out;
}
"""


def action_upload(args) -> dict:
    if not args.file:
        raise SystemExit("upload needs --file")
    path = Path(args.file).resolve()
    if not path.exists():
        raise SystemExit(f"no such file: {path}")

    driver = make_driver(DEFAULT_DOWNLOADS)
    steps = []
    try:
        driver.get("https://studio.youtube.com")
        time.sleep(14)
        upload = by_aria(driver, "Upload videos")
        if upload is None:
            raise SystemExit("YouTube Studio did not show an Upload button (not signed in?)")
        upload.click()
        time.sleep(6)
        steps.append("opened-upload-dialog")

        from selenium.webdriver.common.by import By

        inputs = driver.find_elements(By.CSS_SELECTOR, "input[type=file]")
        if not inputs:
            raise SystemExit("upload dialog has no file input")
        inputs[0].send_keys(str(path))
        steps.append("file-handed-to-input")
        time.sleep(12)

        # Title and description are the first two contenteditable textboxes.
        boxes = driver.execute_script(
            DEEP_JS
            + """
            return deepAll(document, '[contenteditable=true], #textbox', [])
              .filter(e => e.offsetParent)
              .map(e => ({tag:e.tagName, id:e.id, text:(e.innerText||'').slice(0,60)}));
            """
        )
        steps.append({"textboxes": boxes})

        if args.title:
            ok = driver.execute_script(
                DEEP_JS
                + """
                const boxes = deepAll(document, '[contenteditable=true], #textbox', []).filter(e => e.offsetParent);
                const t = boxes.find(e => e.id === 'textbox') || boxes[0];
                if (!t) return false;
                t.focus();
                document.execCommand('selectAll', false, null);
                document.execCommand('insertText', false, arguments[0]);
                return true;
                """,
                args.title,
            )
            steps.append({"title_set": bool(ok)})

        if args.description:
            ok = driver.execute_script(
                DEEP_JS
                + """
                const boxes = deepAll(document, '[contenteditable=true], #textbox', []).filter(e => e.offsetParent);
                const d = boxes.filter(e => e.id === 'textbox')[1] || boxes[1];
                if (!d) return false;
                d.focus();
                document.execCommand('selectAll', false, null);
                document.execCommand('insertText', false, arguments[0]);
                return true;
                """,
                args.description,
            )
            steps.append({"description_set": bool(ok)})

        # Checks step: answer the made-for-kids question, or Next never advances.
        driver.execute_script(
            DEEP_JS
            + """
            const b = deepAll(document, 'button', []).find(e => e.id === 'step-badge-2');
            if (b) b.click();
            """
        )
        time.sleep(4)
        kids = driver.execute_script(
            DEEP_JS
            + """
            for (const r of deepAll(document, 'tp-yt-paper-radio-button', [])) {
              if (r.getAttribute('name') === 'VIDEO_MADE_FOR_KIDS_NOT_MFK') { r.click(); return true; }
            }
            return false;
            """
        )
        steps.append({"not_made_for_kids": bool(kids)})
        time.sleep(2)

        # Visibility step.
        driver.execute_script(
            DEEP_JS
            + """
            const b = deepAll(document, 'button', []).find(e => e.id === 'step-badge-3');
            if (b) b.click();
            """
        )
        time.sleep(4)

        vis = (args.visibility or "private").upper()
        picked = driver.execute_script(
            DEEP_JS
            + """
            const name = arguments[0];
            for (const r of deepAll(document, 'tp-yt-paper-radio-button', [])) {
              if ((r.getAttribute('name') || '').toUpperCase() === name) { r.click(); return name; }
            }
            return null;
            """,
            vis,
        )
        steps.append({"visibility": picked or "not-found"})
        time.sleep(2)

        done = driver.execute_script(
            DEEP_JS
            + """
            const els = deepAll(document, 'button, ytcp-button', []);
            const b = els.find(e => e.id === 'done-button')
                   || els.find(e => ['Save', 'Publish'].includes((e.getAttribute('aria-label') || '').trim()));
            if (!b) return null;
            b.click();
            return (b.innerText || '').trim() || (b.getAttribute('aria-label') || '').trim();
            """
        )
        steps.append({"saved": done or "not-found"})
        time.sleep(8)

        link = driver.execute_script(
            """
            const a = [...document.querySelectorAll('a')].find(a => (a.href || '').includes('youtu.be/'));
            return a ? a.href : '';
            """
        )
        return {
            "ok": True,
            "file": str(path),
            "published": bool(args.publish),
            "link": link,
            "steps": steps,
        }
    finally:
        driver.quit()


def action_extend(args) -> dict:
    """Add a continuation clip at the right edge of the scene timeline."""
    if not args.scene:
        raise SystemExit("extend needs --scene")
    driver = make_driver(DEFAULT_DOWNLOADS)
    try:
        driver.get(args.scene)
        time.sleep(12)

        from selenium.webdriver import ActionChains

        add = by_aria(driver, "Add clip")
        if add is None:
            raise SystemExit("Add clip control not found (is the scene trashed?)")
        ActionChains(driver).move_to_element(add).click().perform()
        steps = ["clicked-add-clip"]
        time.sleep(5)

        if args.prompt:
            box = driver.find_elements("css selector", PROMPT_BOX)
            if box:
                box[0].click()
                box[0].send_keys(args.prompt)
                gen = by_aria(driver, "Start generation")
                if gen is not None:
                    gen.click()
                    steps.append("submitted-continuation-prompt")

        started = time.time()
        while time.time() - started < args.timeout_secs:
            time.sleep(8)
            text = body(driver)
            if "generating" not in text.lower():
                steps.append("generation-settled")
                break
        return {"ok": True, "scene": driver.current_url, "steps": steps}
    finally:
        driver.quit()


def action_run(args) -> dict:
    result = {"ok": True, "steps": []}
    gen = action_generate(args)
    result["generate"] = gen
    result["steps"].append("generate")

    args.scene = gen["scene"]
    dl = action_download(args)
    result["download"] = dl
    result["steps"].append("download")

    if args.upload:
        args.file = dl["file"]
        up = action_upload(args)
        result["upload"] = up
        result["steps"].append("upload")

    return result


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(description="Headless Google Flow driver")
    parser.add_argument("--action", required=True, choices=[
        "refresh-profile", "status", "generate", "download", "upload", "extend", "run",
    ])
    parser.add_argument("--project", default=os.environ.get("FLOW_PROJECT", DEFAULT_PROJECT))
    parser.add_argument("--prompt", default="")
    parser.add_argument("--prompt-file", default="")
    parser.add_argument("--count", type=int, default=1)
    parser.add_argument("--length", type=int, default=8)
    parser.add_argument("--quality", default="720p")
    parser.add_argument("--out", default="")
    parser.add_argument("--scene", default="")
    parser.add_argument("--file", default="")
    parser.add_argument("--title", default="")
    parser.add_argument("--description", default="")
    parser.add_argument("--visibility", default="private")
    parser.add_argument("--publish", action="store_true")
    parser.add_argument("--upload", action="store_true", help="for run: upload after download")
    parser.add_argument("--timeout-secs", type=int, default=900)
    args = parser.parse_args()

    if args.action == "refresh-profile":
        emit(ok=True, **sync_profile())
        return

    ensure_selenium()

    actions = {
        "status": action_status,
        "generate": action_generate,
        "download": action_download,
        "upload": action_upload,
        "extend": action_extend,
        "run": action_run,
    }
    try:
        emit(**actions[args.action](args))
    except SystemExit as error:
        emit(ok=False, error=str(error))
        raise
    except Exception as error:  # noqa: BLE001 - report, do not dump a traceback at the agent
        emit(ok=False, error=f"{type(error).__name__}: {error}")
        raise


if __name__ == "__main__":
    main()
