---
name: flow-headless
description: "Drive Google Flow (text-to-video) in a headless Firefox using the user's real logged-in profile: refresh the profile copy, check login/project status, set video mode, submit a prompt, wait for the clip, download the 720p mp4, and upload to YouTube Studio. No window, no desktop tools, no pointer."
command: "python3 {skill_dir}/scripts/flow_headless.py"
timeout_secs: 3600
parameters:
  type: object
  properties:
    action:
      type: string
      description: "refresh-profile (copy Firefox auth into the headless profile), status (login + project tiles), generate (submit a video prompt and wait), download (scene URL to mp4), upload (mp4 to YouTube Studio), extend (add a continuation clip), run (generate + download [+ upload])."
      enum: ["refresh-profile", "status", "generate", "download", "upload", "extend", "run"]
    project:
      type: string
      description: "Flow project URL. Defaults to the project in FLOW_PROJECT or the last used one."
    prompt:
      type: string
      description: "Prompt text for generate/run."
    prompt_file:
      type: string
      description: "Read the prompt from a file instead of inline text."
    count:
      type: integer
      description: "How many clips to request (x1..x4). Default 1."
    length:
      type: integer
      description: "Clip length in seconds when the model offers a choice. Default 8."
    quality:
      type: string
      description: "Download quality: 720p (default), 1080p, 4k, or gif."
    out:
      type: string
      description: "Directory for downloaded files. Defaults to the skill's downloads/ folder."
    scene:
      type: string
      description: "Scene/edit URL to download or extend."
    file:
      type: string
      description: "Local mp4 to upload."
    title:
      type: string
      description: "YouTube title."
    description:
      type: string
      description: "YouTube description."
    visibility:
      type: string
      description: "YouTube visibility: private (default), unlisted, or public."
    publish:
      type: boolean
      description: "For upload/run: publish instead of leaving a draft. Default false."
    timeout_secs:
      type: integer
      description: "How long to wait for a generation. Default 900."
---

# Flow headless

Drives Google Flow in **headless Firefox** using a trimmed copy of the user's real
Firefox profile, so the Google/YouTube login comes along without re-authentication.

The full recipe and selector map live in
`Documents/stickman-videos/congrats-youre-hired/FLOW_HEADLESS.md`.

## Actions

| Action | What it does |
|---|---|
| `refresh-profile` | Copies `prefs.js`, `cookies.sqlite`, `key4.db`, `logins.json`, `cert9.db`, `pkcs11.txt`, `permissions.sqlite` from the newest Firefox profile into `profile/`. Cookies are copied with SQLite's online backup API, so a running Firefox is safe. |
| `status` | Opens the project headless and reports the signed-in account, tile count, and trash count. |
| `generate` | Sets Video mode + count, types the prompt, submits, and waits until a new `Generated video thumbnail` appears. Returns the scene URL. |
| `download` | Opens the scene editor, clicks Download, picks the requested quality, and waits for the file. The first menu item is the GIF, so 720p is selected explicitly. |
| `upload` | Opens YouTube Studio, hands the mp4 to the hidden file input, fills title/description, sets visibility, and either saves a draft or publishes. |
| `extend` | Opens the scene and clicks the `+` at the clip's right edge (aria "Add clip"), then waits for the duration to grow. Experimental. |
| `run` | `generate` → `download` → optional `upload`. |

## Notes

- The prompt bar resets to image mode (Nano Banana) between sessions. `generate` always
  sets Video and asserts the chip reads `Video · 720p · …` before submitting, so it can
  never silently spend credits on an image.
- Flow's tile viewer only opens on a real pointer click (`ActionChains`), not
  `element.click()`.
- YouTube Studio is Polymer with shadow DOM; menus and the delete confirmation need a
  recursive `shadowRoot` walk.
- The profile directory and downloads are gitignored. Never commit `profile/`.
- Default is **not** to publish: `upload` saves a draft unless `publish: true`.
