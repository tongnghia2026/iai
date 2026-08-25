// IAI Bridge — background service worker.
//
// Holds the WebSocket to iai's localhost server (ws://127.0.0.1:47821). Lives in
// the background (extension context) NOT the content script, so the page's CSP
// can't block the localhost connection. Relays `edit` commands to the Gemini tab's
// content script, and forwards the content script's status/result back to iai.
//
// MV3 service workers get killed when idle, so a chrome.alarms tick re-opens the
// socket; the open socket itself also keeps the worker alive while a job runs.

const WS_URL = "ws://127.0.0.1:47821";
let ws = null;
let connecting = false;
const pendingJobs = new Map();
let lastStatus = "Chua ket noi IAI";

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function getToken() {
  const { token } = await chrome.storage.local.get("token");
  return token || "";
}

function wsOpen() {
  return ws && ws.readyState === WebSocket.OPEN;
}

async function connect() {
  if (connecting || wsOpen()) return;
  connecting = true;
  const token = await getToken();
  if (!token) {
    connecting = false;
    lastStatus = "Chua co token - dan token trong popup";
    console.log("[IAI]", lastStatus);
    return;
  }
  lastStatus = "Dang ket noi IAI...";
  console.log("[IAI] connecting to", WS_URL);
  try {
    ws = new WebSocket(WS_URL);
  } catch (e) {
    connecting = false;
    console.log("[IAI] WebSocket ctor failed", e);
    return;
  }
  ws.onopen = () => {
    connecting = false;
    lastStatus = "Socket open - dang gui hello";
    console.log("[IAI] socket open -> hello");
    // One background relays to whichever site each edit targets; "web" is just a
    // label for IAI's connection status line.
    sendWs({ type: "hello", token, site: "web" });
  };
  ws.onmessage = (ev) => {
    let msg;
    try {
      msg = JSON.parse(ev.data);
    } catch {
      return;
    }
    if (msg.type === "status") {
      lastStatus = msg.message || "IAI status";
      console.log("[IAI] status from app:", lastStatus);
      return;
    }
    if (msg.type === "error") {
      lastStatus = msg.message || "IAI error";
      console.log("[IAI] error from app:", lastStatus);
      return;
    }
    if (msg.type === "edit") relayToContent(msg);
    if (msg.type === "cancel") cancelJob(msg.id);
  };
  ws.onclose = () => {
    if (lastStatus === "Socket open - dang gui hello") {
      lastStatus = "Socket bi dong sau hello - token sai hoac IAI restart";
    }
    console.log("[IAI] socket closed");
    ws = null;
    connecting = false;
  };
  ws.onerror = (e) => {
    lastStatus = "Khong ket noi duoc IAI tai " + WS_URL + " - IAI chua chay hoac bridge chua listen";
    console.log("[IAI] socket error (is IAI running? token correct?)", e);
    try {
      ws && ws.close();
    } catch {}
    ws = null;
    connecting = false;
  };
}

function sendWs(obj) {
  if (wsOpen()) {
    try {
      ws.send(JSON.stringify(obj));
    } catch {}
  }
}

function failWs(id, message) {
  sendWs({ type: "status", id, message });
  sendWs({ type: "error", id, message });
}

async function ensureOffscreen() {
  if (await chrome.offscreen.hasDocument()) return;
  await chrome.offscreen.createDocument({
    url: "offscreen.html",
    reasons: ["CLIPBOARD"],
    justification: "Copy IAI image and prompt to the clipboard for pasting into Gemini",
  });
}

async function clipboardImage(image) {
  await ensureOffscreen();
  return await chrome.runtime.sendMessage({ target: "offscreen", type: "clipboardImage", image });
}

async function clipboardText(text) {
  await ensureOffscreen();
  return await chrome.runtime.sendMessage({ target: "offscreen", type: "clipboardText", text });
}

function trackJob(id, tabId) {
  clearJob(id);
  const timer = setTimeout(() => {
    failWs(id, "Het thoi gian cho Gemini phan hoi - da huy lenh");
    clearJob(id);
  }, 240000);
  pendingJobs.set(id, { tabId, timer });
}

function clearJob(id) {
  const job = pendingJobs.get(id);
  if (job && job.timer) clearTimeout(job.timer);
  pendingJobs.delete(id);
  // Detach the debugger once the last job for this tab is done.
  if (job && job.tabId != null) {
    const stillUsed = [...pendingJobs.values()].some((j) => j.tabId === job.tabId);
    if (!stillUsed) detachTab(job.tabId);
  }
}

async function cancelJob(id) {
  const job = pendingJobs.get(id);
  clearJob(id);
  if (job && job.tabId) {
    try {
      await chrome.tabs.sendMessage(job.tabId, { type: "cancel", id });
    } catch {}
  }
  sendWs({ type: "status", id, message: "Da huy lenh dang cho" });
}

function jobTabId(id, sender) {
  const job = pendingJobs.get(id);
  return (job && job.tabId) || (sender && sender.tab && sender.tab.id);
}

async function focusTab(tabId) {
  const tab = await chrome.tabs.get(tabId);
  if (tab.windowId != null) {
    await chrome.windows.update(tab.windowId, { focused: true });
  }
  await chrome.tabs.update(tabId, { active: true });
  await sleep(180);
}

// Keep the debugger attached for the WHOLE job (attach on first use, detach when
// the job ends). Attaching/detaching per keystroke races — a later attach can fire
// before the previous detach completes, fail, and the command then runs un-attached.
const attachedTabs = new Set();

async function ensureAttached(tabId) {
  if (attachedTabs.has(tabId)) return;
  try {
    await chrome.debugger.attach({ tabId }, "1.3");
    attachedTabs.add(tabId);
  } catch (e) {
    // Already attached (by us, or DevTools open) → treat as attached.
    if (String(e).includes("already attached") || String(e).includes("Another debugger")) {
      attachedTabs.add(tabId);
    } else {
      throw e;
    }
  }
}

async function detachTab(tabId) {
  if (!attachedTabs.has(tabId)) return;
  attachedTabs.delete(tabId);
  try {
    await chrome.debugger.detach({ tabId });
  } catch {}
}

async function withDebugger(tabId, fn) {
  await ensureAttached(tabId);
  return await fn({ tabId });
}

// If the user dismisses the "IAI Bridge is debugging this browser" banner, Chrome
// detaches — forget the tab so the next op re-attaches.
chrome.debugger.onDetach.addListener((source) => {
  if (source && source.tabId != null) attachedTabs.delete(source.tabId);
});

async function realPaste(tabId) {
  await focusTab(tabId);
  await withDebugger(tabId, async (target) => {
    await chrome.debugger.sendCommand(target, "Input.dispatchKeyEvent", {
      type: "keyDown",
      key: "Control",
      code: "ControlLeft",
      windowsVirtualKeyCode: 17,
      nativeVirtualKeyCode: 17,
      modifiers: 2,
    });
    await chrome.debugger.sendCommand(target, "Input.dispatchKeyEvent", {
      type: "keyDown",
      key: "v",
      code: "KeyV",
      windowsVirtualKeyCode: 86,
      nativeVirtualKeyCode: 86,
      modifiers: 2,
    });
    await chrome.debugger.sendCommand(target, "Input.dispatchKeyEvent", {
      type: "keyUp",
      key: "v",
      code: "KeyV",
      windowsVirtualKeyCode: 86,
      nativeVirtualKeyCode: 86,
      modifiers: 2,
    });
    await chrome.debugger.sendCommand(target, "Input.dispatchKeyEvent", {
      type: "keyUp",
      key: "Control",
      code: "ControlLeft",
      windowsVirtualKeyCode: 17,
      nativeVirtualKeyCode: 17,
      modifiers: 0,
    });
  });
}

async function realClick(tabId, x, y) {
  await focusTab(tabId);
  await withDebugger(tabId, async (target) => {
    await chrome.debugger.sendCommand(target, "Input.dispatchMouseEvent", {
      type: "mouseMoved",
      x,
      y,
      button: "none",
    });
    await chrome.debugger.sendCommand(target, "Input.dispatchMouseEvent", {
      type: "mousePressed",
      x,
      y,
      button: "left",
      clickCount: 1,
    });
    await chrome.debugger.sendCommand(target, "Input.dispatchMouseEvent", {
      type: "mouseReleased",
      x,
      y,
      button: "left",
      clickCount: 1,
    });
  });
}

async function realScroll(tabId, x, y) {
  // Wheel input is only reliably delivered to the page when its tab is active.
  // The edit workflow already owns this tab, so re-assert focus before each
  // scrolling burst in case the browser moved focus while generation ran.
  await focusTab(tabId);
  await withDebugger(tabId, async (target) => {
    // A trusted wheel event is routed by Chromium to the actual scroll container
    // under this point. This keeps working when Gemini replaces/virtualizes its
    // conversation DOM and ignores content-script scrollTop changes.
    for (let i = 0; i < 3; i++) {
      await chrome.debugger.sendCommand(target, "Input.dispatchMouseEvent", {
        type: "mouseWheel",
        x,
        y,
        deltaX: 0,
        deltaY: 4096,
      });
      await sleep(40);
    }
  });
}

async function realType(tabId, text) {
  await focusTab(tabId);
  await withDebugger(tabId, async (target) => {
    await chrome.debugger.sendCommand(target, "Input.insertText", {
      text: text || "",
    });
  });
}

async function realEnter(tabId) {
  await focusTab(tabId);
  await withDebugger(tabId, async (target) => {
    await chrome.debugger.sendCommand(target, "Input.dispatchKeyEvent", {
      type: "keyDown",
      key: "Enter",
      code: "Enter",
      windowsVirtualKeyCode: 13,
      nativeVirtualKeyCode: 13,
    });
    await chrome.debugger.sendCommand(target, "Input.dispatchKeyEvent", {
      type: "keyUp",
      key: "Enter",
      code: "Enter",
      windowsVirtualKeyCode: 13,
      nativeVirtualKeyCode: 13,
    });
  });
}

// Per-site routing: which tab URLs to look for, the human label, and where to
// send the user if no tab is open. `site` comes from IAI's edit message
// ("gemini" / "chatgpt"); default to Gemini for older IAI builds.
const SITES = {
  gemini: {
    label: "Gemini",
    urls: ["https://gemini.google.com/*"],
    openHint: "gemini.google.com",
  },
  chatgpt: {
    label: "ChatGPT",
    urls: ["https://chatgpt.com/*", "https://chat.openai.com/*"],
    openHint: "chatgpt.com",
  },
};

function chooseTab(tabs) {
  return tabs
    .slice()
    .sort((a, b) => {
      if (a.active !== b.active) return a.active ? -1 : 1;
      if (a.highlighted !== b.highlighted) return a.highlighted ? -1 : 1;
      return (b.lastAccessed || 0) - (a.lastAccessed || 0);
    })[0];
}

async function relayToContent(msg) {
  const site = SITES[msg.site] || SITES.gemini;
  const tabs = await chrome.tabs.query({ url: site.urls });
  if (!tabs.length) {
    failWs(msg.id, "Chua mo tab " + site.label + " - hay mo " + site.openHint);
    return;
  }
  const tab = chooseTab(tabs);
  const tabId = tab && tab.id;
  if (!tabId) {
    failWs(msg.id, "Khong tim thay tab " + site.label + " hop le");
    return;
  }
  sendWs({ type: "status", id: msg.id, message: "Dang gui lenh toi tab " + site.label + ": " + (tab.title || tab.url || tabId) });
  trackJob(msg.id, tabId);
  try {
    await chrome.tabs.sendMessage(tabId, msg);
  } catch (e) {
    // The content script isn't in this tab yet (the tab was open before the
    // extension loaded). Inject it on demand, then retry — no manual reload needed.
    try {
      await chrome.scripting.executeScript({ target: { tabId }, files: ["content-web.js"] });
      await chrome.tabs.sendMessage(tabId, msg);
    } catch (e2) {
      clearJob(msg.id);
      failWs(msg.id, "Khong gui duoc toi tab " + site.label + " - thu reload tab " + site.openHint + ". (" + e2 + ")");
      return;
    }
  }
}

// Content script → background. `fetchImage` is fetched HERE (background has the
// host permissions for googleusercontent, the content script doesn't); everything
// else (status / result / hello) is forwarded to iai over the socket.
chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (!msg || !msg.type) return;
  if (msg.target === "offscreen") return;
  if (msg.type === "reconnect") {
    // Popup saved a (new) token → drop any stale socket and connect now.
    try {
      ws && ws.close();
    } catch {}
    ws = null;
    connecting = false;
    connect();
    return;
  }
  if (msg.type === "getStatus") {
    sendResponse({ connected: wsOpen(), status: lastStatus, url: WS_URL });
    return; // sync response
  }
  if (msg.type === "fetchImage") {
    fetch(msg.src, { credentials: "include" })
      .then((r) => {
        const ct = (r.headers && r.headers.get("content-type")) || "";
        // Some CDNs serve the image with an empty or generic content-type; only
        // reject when it's explicitly a NON-image (e.g. a redirect to HTML login).
        if (!r.ok || (ct && !/^image\//i.test(ct))) throw new Error("not image: " + r.status + " " + ct);
        return r.blob();
      })
      .then((b) => {
        if (b.type && !/^image\//i.test(b.type)) {
          sendResponse({ image: null });
          return;
        }
        const fr = new FileReader();
        fr.onloadend = () => {
          const s = String(fr.result);
          const i = s.indexOf("base64,");
          sendResponse({ image: i >= 0 ? s.slice(i + 7) : s });
        };
        fr.readAsDataURL(b);
      })
      .catch(() => sendResponse({ image: null }));
    return true; // async sendResponse
  }
  if (msg.type === "clipboardImage") {
    clipboardImage(msg.image)
      .then((r) => sendResponse(r || { ok: true }))
      .catch((e) => sendResponse({ ok: false, error: String(e) }));
    return true;
  }
  if (msg.type === "clipboardText") {
    clipboardText(msg.text)
      .then((r) => sendResponse(r || { ok: true }))
      .catch((e) => sendResponse({ ok: false, error: String(e) }));
    return true;
  }
  if (msg.type === "realPaste") {
    const tabId = jobTabId(msg.id, sender);
    if (!tabId) {
      sendResponse({ ok: false, error: "missing tab id" });
      return true;
    }
    realPaste(tabId)
      .then(() => sendResponse({ ok: true }))
      .catch((e) => sendResponse({ ok: false, error: String(e) }));
    return true;
  }
  if (msg.type === "realClick") {
    const tabId = jobTabId(msg.id, sender);
    if (!tabId) {
      sendResponse({ ok: false, error: "missing tab id" });
      return true;
    }
    realClick(tabId, msg.x, msg.y)
      .then(() => sendResponse({ ok: true }))
      .catch((e) => sendResponse({ ok: false, error: String(e) }));
    return true;
  }
  if (msg.type === "realScroll") {
    const tabId = jobTabId(msg.id, sender);
    if (!tabId) {
      sendResponse({ ok: false, error: "missing tab id" });
      return true;
    }
    realScroll(tabId, msg.x, msg.y)
      .then(() => sendResponse({ ok: true }))
      .catch((e) => sendResponse({ ok: false, error: String(e) }));
    return true;
  }
  if (msg.type === "realType") {
    const tabId = jobTabId(msg.id, sender);
    if (!tabId) {
      sendResponse({ ok: false, error: "missing tab id" });
      return true;
    }
    realType(tabId, msg.text)
      .then(() => sendResponse({ ok: true }))
      .catch((e) => sendResponse({ ok: false, error: String(e) }));
    return true;
  }
  if (msg.type === "realEnter") {
    const tabId = jobTabId(msg.id, sender);
    if (!tabId) {
      sendResponse({ ok: false, error: "missing tab id" });
      return true;
    }
    realEnter(tabId)
      .then(() => sendResponse({ ok: true }))
      .catch((e) => sendResponse({ ok: false, error: String(e) }));
    return true;
  }
  if (msg.type === "result" || msg.type === "error" || msg.type === "result_clipboard") {
    clearJob(msg.id);
  }
  sendWs(msg);
});

// Hosts that a running job's tab is allowed to stay on (any supported web AI).
const AI_HOST_PREFIXES = [
  "https://gemini.google.com/",
  "https://chatgpt.com/",
  "https://chat.openai.com/",
];

chrome.tabs.onRemoved.addListener((tabId) => {
  for (const [id, job] of pendingJobs) {
    if (job.tabId === tabId) {
      failWs(id, "Tab AI da dong - da huy lenh dang cho");
      clearJob(id);
    }
  }
});

chrome.tabs.onUpdated.addListener((tabId, changeInfo) => {
  if (!changeInfo.url) return;
  if (AI_HOST_PREFIXES.some((p) => changeInfo.url.startsWith(p))) return;
  for (const [id, job] of pendingJobs) {
    if (job.tabId === tabId) {
      failWs(id, "Tab AI da chuyen trang - da huy lenh dang cho");
      clearJob(id);
    }
  }
});

// Reconnect heartbeat (service worker may be killed/woken).
chrome.alarms.create("iai-reconnect", { periodInMinutes: 0.5 });
chrome.alarms.onAlarm.addListener(() => connect());
chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(connect);
connect();




