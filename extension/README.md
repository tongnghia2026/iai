# IAI Bridge (browser extension)

Drives your **own** logged-in Gemini / ChatGPT tab to edit images for IAI — for
free, using your normal browser session. Unlike IAI's old embedded webview, this
runs in your real **Chromium** browser (Chrome / Edge / Brave), so it works the
same on **Linux** too. (Firefox is experimental — see below.)

## Status

End to end: IAI ↔ extension connect, and a "Web" run **attaches the image + fills
the prompt** into your chat composer, **auto-submits**, and **grabs the generated
result** back into a layer. Works on both **Gemini** (gemini.google.com) and
**ChatGPT** (chatgpt.com / chat.openai.com) — one site-agnostic content script
(`content-web.js`) driven by generic role/label heuristics; IAI's AI panel picks
the site and `background.js` routes each edit to the matching tab by `site`.

While waiting for the result the extension **auto-scrolls the chat to the
bottom** and **forces lazy images to load** — Gemini renders the generated image
below the viewport as a lazy `<img>`, so without this the grab only fired after
the user scrolled it into view manually.

Sending is a **retry loop** (up to ~45s): the site disables Send while the
attachment uploads, so the extension waits for the button to unlock and retries
Enter/click until the prompt leaves the composer. IAI also places the upload
image on the **OS clipboard natively** when it dispatches the job
(`"clipboard": true` in the edit message), so the flaky offscreen-document copy
is only a fallback for older IAI builds — and the image is attached via a single
Ctrl+V (no DOM backup) so it never lands **twice**.

> The DOM automation targets each site's current page; if the layout changes (or
> the image doesn't auto-attach), use the **Ctrl+V** fallback (the image is also
> put on your clipboard) and re-check the heuristics in `content-web.js`.

## Install (Chrome / Edge / Brave — load unpacked)

1. Open `chrome://extensions`, enable **Developer mode** (top right).
2. **Load unpacked** → select this `extension/` folder.
3. Start IAI, open the **AI panel**, pick a **Web** source (Gemini **or**
   ChatGPT) — it shows a **token**.
4. Click the IAI Bridge toolbar icon → paste the token → **Lưu token**.
5. Open <https://gemini.google.com/> or <https://chatgpt.com/> and log in (match
   the source you picked). The extension connects to IAI at
   `ws://127.0.0.1:47821`; the AI panel should show **Extension: đã kết nối**.

## How it talks to IAI

- IAI runs a localhost-only WebSocket server on port **47821**.
- The extension **background** holds the socket (page CSP can't block localhost
  from the extension context) and relays each edit to the **content script** in
  the tab that matches the edit's `site` (Gemini or ChatGPT).
- A per-session **token** (shown in IAI) gates the connection so a random web page
  can't connect to the port and read your canvas.

## Protocol (JSON)

- IAI → ext: `{ "type": "edit", "id", "site", "prompt", "image": "<base64 png>", "clipboard": <bool> }`
  (`site` = `"gemini"` | `"chatgpt"`; `clipboard: true` = IAI already put the image
  on the OS clipboard, so the extension pastes with Ctrl+V)
- ext → IAI: `{ "type": "hello", "token", "site" }`,
  `{ "type": "status", "id?", "message" }`,
  `{ "type": "result", "id", "image": "<base64 png>" }`

## Firefox (experimental)

**Untested and unsupported.** Firefox's MV3 background model differs and the DOM
heuristics have not been verified there, so expect breakage — Chrome / Edge /
Brave are the supported browsers. To try it anyway: `about:debugging` → This
Firefox → Load Temporary Add-on → pick `manifest.json`; you will likely need to
adjust the background / service-worker wiring first.
