// IAI Bridge — web AI content script (Gemini or ChatGPT).
//
// One site-agnostic script drives whichever chat page it is injected into: on an
// `edit` command it attaches the canvas image to the composer, fills the prompt,
// and arms a MutationObserver for the generated result; when the result appears it
// is read to base64 and sent back to iai via the background socket. The composer /
// send-button / result detection is done by generic role+label heuristics (not
// site-specific selectors), so the same code works on gemini.google.com and
// chatgpt.com. background.js registers it per host and routes each edit by `site`.

(function () {
  "use strict";
  var activeJobs = {};
  // Which site we are on, for user-facing status text only (all DOM logic is
  // heuristic and site-agnostic).
  var IS_GEMINI = /gemini\.google\.com/.test(location.host);
  var IS_CHATGPT = !IS_GEMINI;
  var SITE = IS_GEMINI ? "Gemini" : "ChatGPT";

  // Walk light + open shadow DOM — Gemini's file input lives in a shadow root.
  function deepQuery(sel) {
    var out = [];
    function walk(root) {
      try {
        Array.prototype.forEach.call(root.querySelectorAll(sel), function (e) {
          out.push(e);
        });
      } catch (e) {}
      var all;
      try {
        all = root.querySelectorAll("*");
      } catch (e) {
        all = [];
      }
      for (var i = 0; i < all.length; i++) {
        if (all[i].shadowRoot) walk(all[i].shadowRoot);
      }
    }
    walk(document);
    return out;
  }

  function uniqueElements(list) {
    var out = [];
    for (var i = 0; i < list.length; i++) {
      if (list[i] && out.indexOf(list[i]) < 0) out.push(list[i]);
    }
    return out;
  }

  function composerSelectors() {
    if (IS_CHATGPT) {
      return [
        "#prompt-textarea",
        '[data-testid="composer-text-input"]',
        '[data-testid="prompt-textarea"]',
        'textarea[data-testid="prompt-textarea"]',
        'textarea[name="prompt-textarea"]',
        '.ProseMirror[contenteditable="true"]',
        '[contenteditable="true"][role="textbox"]',
        '[contenteditable="true"][data-lexical-editor="true"]',
        '[contenteditable]:not([contenteditable="false"])',
        '[contenteditable="true"]',
        "textarea",
        '[role="textbox"]',
      ];
    }
    return ['[contenteditable]:not([contenteditable="false"])', '[contenteditable="true"]', "textarea", '[role="textbox"]'];
  }

  function rejectComposerCandidate(el) {
    if (!el || !isVisible(el)) return true;
    if (el.disabled || el.readOnly || el.getAttribute("aria-disabled") === "true") return true;
    if (el.closest && el.closest('[aria-hidden="true"],[inert]')) return true;
    if (IS_CHATGPT && el.closest && el.closest('nav,aside,[role="navigation"]')) return true;
    return false;
  }

  function composerScore(el) {
    var score = 0;
    var r = el.getBoundingClientRect();
    var attrs = normText(
      [
        el.id,
        el.className,
        el.getAttribute("data-testid"),
        el.getAttribute("name"),
        el.getAttribute("aria-label"),
        el.getAttribute("placeholder"),
      ].join(" ")
    );
    if (/prompt-textarea|composer-text-input|prosemirror/.test(attrs)) score += 1000;
    if (el.isContentEditable) score += 120;
    if (el.tagName === "TEXTAREA") score += 100;
    if (el.getAttribute("role") === "textbox") score += 80;
    if (el.closest && el.closest("form")) score += 120;
    if (el.closest && el.closest("main")) score += 60;
    if (r.bottom > window.innerHeight * 0.45) score += 180;
    score += Math.max(0, r.bottom);
    return score;
  }

  function composer() {
    var candidates = [];
    var selectors = composerSelectors();
    for (var i = 0; i < selectors.length; i++) {
      candidates = candidates.concat(deepQuery(selectors[i]));
    }
    candidates = uniqueElements(candidates).filter(function (el) {
      return !rejectComposerCandidate(el);
    });
    candidates.sort(function (a, b) {
      return composerScore(b) - composerScore(a);
    });
    return candidates[0] || null;
  }

  function composerBox() {
    var ce = composer();
    if (!ce) return null;
    if (IS_CHATGPT && ce.closest) {
      var box = ce.closest('[data-testid*="composer"],form');
      if (box) return box;
    }
    var b = ce;
    for (var k = 0; k < 3 && b.parentElement; k++) b = b.parentElement;
    return b;
  }

  function imageKey(img) {
    return img.currentSrc || img.src || "";
  }

  function composerImages() {
    var box = composerBox() || document.body;
    return Array.prototype.filter.call(box.querySelectorAll("img"), function (img) {
      return (img.naturalWidth || img.width || 0) > 24 && (img.naturalHeight || img.height || 0) > 24;
    });
  }

  function waitForAttachment(before, timeoutMs, cb) {
    var started = Date.now();
    var timer = setInterval(function () {
      var imgs = composerImages().filter(function (img) {
        return !before.has(imageKey(img));
      });
      if (imgs.length) {
        clearInterval(timer);
        cb(true);
      } else if (Date.now() - started >= timeoutMs) {
        clearInterval(timer);
        cb(false);
      }
    }, 350);
    return function () {
      clearInterval(timer);
    };
  }

  function hasNewAttachment(before) {
    return composerImages().some(function (img) {
      return !before.has(imageKey(img));
    });
  }

  function isDisabled(el) {
    return !!(el.disabled || el.getAttribute("aria-disabled") === "true" || el.classList.contains("disabled"));
  }

  function isVisible(el) {
    if (!el) return false;
    var r;
    try {
      r = el.getBoundingClientRect();
    } catch (e) {
      return false;
    }
    return !!r && r.width > 0 && r.height > 0;
  }

  function normText(s) {
    try {
      return String(s || "")
        .normalize("NFD")
        .replace(/[\u0300-\u036f]/g, "")
        .toLowerCase();
    } catch (e) {
      return String(s || "").toLowerCase();
    }
  }

  function buttonText(b) {
    return [
      b.getAttribute("aria-label"),
      b.getAttribute("title"),
      b.getAttribute("data-tooltip"),
      b.getAttribute("mattooltip"),
      b.getAttribute("data-testid"),
      b.getAttribute("data-test-id"),
      b.id,
      b.textContent,
    ]
      .filter(Boolean)
      .join(" ");
  }

  function looksLikeSendButton(b) {
    var label = normText(buttonText(b));
    return /send|submit|gui|arrow_upward|send_message|send-button|composer-send|composer-submit/.test(label);
  }

  function findSendButton(includeDisabled) {
    var direct = deepQuery(
      [
        'button[data-testid="send-button"]',
        'button[data-testid="composer-send-button"]',
        'button[data-testid="composer-submit-button"]',
        'button[data-test-id="send-button"]',
      ].join(",")
    );
    for (var d = 0; d < direct.length; d++) {
      if (isVisible(direct[d]) && (includeDisabled || !isDisabled(direct[d]))) return direct[d];
    }

    var buttons = deepQuery('button,[role="button"]');
    for (var i = 0; i < buttons.length; i++) {
      var b = buttons[i];
      if (!isVisible(b)) continue;
      if (!includeDisabled && isDisabled(b)) continue;
      if (looksLikeSendButton(b)) return b;
      var label = [
        b.getAttribute("aria-label"),
        b.getAttribute("title"),
        b.getAttribute("data-tooltip"),
        b.getAttribute("mattooltip"),
        b.textContent,
      ]
        .filter(Boolean)
        .join(" ");
      if (/send|submit|gửi|gui|arrow_upward|send_message/i.test(label)) return b;
    }
    return null;
  }

  function clickSend() {
    var btn = findSendButton();
    if (!btn) return false;
    btn.click();
    return true;
  }

  function placeCaretAtEnd(ce) {
    if (!ce) return;
    try {
      if (ce.tagName === "TEXTAREA" || ce.tagName === "INPUT") {
        var len = ce.value ? ce.value.length : 0;
        ce.setSelectionRange(len, len);
        return;
      }
      var range = document.createRange();
      range.selectNodeContents(ce);
      range.collapse(false);
      var sel = window.getSelection();
      sel.removeAllRanges();
      sel.addRange(range);
    } catch (e) {}
  }

  function focusComposer() {
    var ce = composer();
    if (!ce) return false;
    try {
      ce.scrollIntoView({ block: "center", inline: "center" });
    } catch (e) {}
    try {
      ce.focus({ preventScroll: true });
    } catch (e) {
      ce.focus();
    }
    placeCaretAtEnd(ce);
    return true;
  }

  function sendButtonCenter() {
    var btn = findSendButton();
    if (!btn) return null;
    try {
      btn.scrollIntoView({ block: "center", inline: "center" });
    } catch (e) {}
    var r = btn.getBoundingClientRect();
    if (!r || r.width <= 0 || r.height <= 0) return null;
    return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  }

  // Click point inside the composer's TEXT element (the contenteditable/textarea
  // itself — the image-attachment chip is a separate element, so its center is safe).
  function composerCenter() {
    var ce = composer();
    if (!ce) return null;
    try {
      ce.scrollIntoView({ block: "center", inline: "center" });
    } catch (e) {}
    var r = ce.getBoundingClientRect();
    if (!r || r.width <= 0 || r.height <= 0) return null;
    return {
      x: Math.max(r.left + 8, Math.min(r.right - 8, r.left + r.width / 2)),
      y: Math.max(r.top + 8, Math.min(r.bottom - 8, r.bottom - 14)),
    };
  }
  // The generated result carries download/copy/save controls in an ancestor; the
  // echoed source image and the composer preview do not — the reliable signal.
  // Labels cover both Gemini (Tải xuống / kích thước đầy đủ / Sao chép hình ảnh)
  // and ChatGPT (Download / Save image), plus English fallbacks.
  function hasResultControls(img) {
    var n = img;
    for (var h = 0; h < 9 && n; h++) {
      try {
        var els = n.querySelectorAll("button,[role=button],[aria-label],[mattooltip],a[download]");
        for (var i = 0; i < els.length; i++) {
          if (els[i].hasAttribute && els[i].hasAttribute("download")) return true;
          var raw =
            (els[i].getAttribute("aria-label") ||
              els[i].getAttribute("mattooltip") ||
              els[i].getAttribute("title") ||
              "") + "";
          var label = normText(raw + " " + buttonText(els[i]));
          if (
            /tai.*xuong|download|tai ve|save image|luu (anh|hinh)|full.?size|kich thuoc day du|sao chep hinh anh|sao chep anh|copy image|download-button/.test(
              label
            )
          ) {
            return true;
          }
        }
      } catch (e) {}
      n = n.parentElement;
    }
    return false;
  }

  function nodeTextMatches(el, re) {
    if (!el) return false;
    var text = "";
    try {
      text = (el.innerText || el.textContent || "").replace(/\s+/g, " ").trim();
    } catch (e) {
      text = "";
    }
    return !!text && re.test(text);
  }

  function chatGptTurnRoot(img) {
    var n = img;
    for (var h = 0; h < 14 && n && n !== document.body && n !== document.documentElement; h++) {
      if (
        n.matches &&
        (n.matches("article") ||
          n.matches('[data-testid*="conversation-turn"]') ||
          n.getAttribute("data-message-author-role") === "assistant")
      ) {
        return n;
      }
      n = n.parentElement;
    }
    return img && img.parentElement ? img.parentElement : null;
  }

  function chatGptTurnRole(img) {
    var n = img;
    for (var h = 0; h < 14 && n && n !== document.body && n !== document.documentElement; h++) {
      var role = n.getAttribute && n.getAttribute("data-message-author-role");
      if (role) return role;
      if (n.matches && n.matches('[data-testid*="conversation-turn"]')) {
        var withRole = n.querySelector && n.querySelector("[data-message-author-role]");
        if (withRole) return withRole.getAttribute("data-message-author-role") || "";
      }
      n = n.parentElement;
    }
    return "";
  }

  // ChatGPT sometimes shows "Worked for ...", but not always. Treat it as a
  // strong completion signal when present, not as the only way to finish.
  function hasChatGptDoneMarker(img) {
    var doneRe = /\bWorked for\b/i;
    var n = img;
    for (var h = 0; h < 14 && n && n !== document.body && n !== document.documentElement; h++) {
      if (nodeTextMatches(n, doneRe)) return true;

      var prev = n.previousElementSibling;
      for (var p = 0; p < 4 && prev; p++) {
        if (nodeTextMatches(prev, doneRe)) return true;
        prev = prev.previousElementSibling;
      }

      if (
        n.matches &&
        (n.matches("article") ||
          n.matches('[data-testid*="conversation-turn"]') ||
          n.getAttribute("data-message-author-role") === "assistant")
      ) {
        break;
      }
      n = n.parentElement;
    }
    return false;
  }

  function chatGptStillGenerating(img) {
    var root = chatGptTurnRoot(img) || document.body;
    var text = "";
    try {
      text = normText(root.innerText || root.textContent || "");
    } catch (e) {
      text = "";
    }
    if (/dang suy nghi|dang tao|dang xu ly|cho chut|creating|generating|thinking/.test(text)) return true;

    try {
      var controls = root.querySelectorAll("button,[role=button],[aria-label],[data-testid]");
      for (var i = 0; i < controls.length; i++) {
        var label = normText(buttonText(controls[i]));
        if (/stop|stop-button|cancel response|dung|tam dung/.test(label)) return true;
      }
    } catch (e) {}
    return false;
  }

  function chatGptPageStillGenerating() {
    var text = "";
    try {
      text = normText(document.body.innerText || document.body.textContent || "");
    } catch (e) {
      text = "";
    }
    if (/dang suy nghi|dang tao|dang xu ly|cho chut|creating|generating|thinking/.test(text)) return true;

    try {
      var controls = document.querySelectorAll("button,[role=button],[aria-label],[data-testid]");
      for (var i = 0; i < controls.length; i++) {
        var label = normText(buttonText(controls[i]));
        if (/stop|stop-button|cancel response|dung|tam dung/.test(label)) return true;
      }
    } catch (e) {}
    return false;
  }

  function isChatGptResultReady(img) {
    if (hasChatGptDoneMarker(img)) return true;
    return isLoadedResult(img) && !chatGptStillGenerating(img);
  }

  // Candidate check is separate from the loaded check: Gemini lazy-loads the
  // result image below the viewport, so naturalWidth stays 0 until it is scrolled
  // into view — a size gate here would silently miss it. Loaded-but-small images
  // (toolbar icons, avatars) are still rejected before the ancestor walk.
  function isResultCandidate(img) {
    if (img.complete && img.naturalWidth > 0 && (img.naturalWidth < 256 || img.naturalHeight < 256)) {
      return false;
    }
    var cb = composerBox();
    if (cb && cb.contains(img)) return false;
    if (IS_CHATGPT) {
      var role = chatGptTurnRole(img);
      if (role && role !== "assistant") return false;
      if (!isChatGptResultReady(img)) return false;
      return true;
    }
    if (!hasResultControls(img)) return false;
    return true;
  }

  function isLoadedResult(img) {
    return img.complete && img.naturalWidth >= 256 && img.naturalHeight >= 256;
  }

  function imagePageBottom(img) {
    try {
      var r = img.getBoundingClientRect();
      return r.bottom + (window.scrollY || document.documentElement.scrollTop || 0);
    } catch (e) {
      return 0;
    }
  }

  // The conversation pane = the largest scrollable element on the page (the
  // sidebar also scrolls but is smaller). Recomputed per call — Gemini rebuilds
  // the DOM between conversations.
  function chatScroller() {
    var best = null;
    var bestArea = 0;
    var all = document.querySelectorAll("div,main,section");
    for (var i = 0; i < all.length; i++) {
      var el = all[i];
      if (el.scrollHeight <= el.clientHeight + 40) continue;
      var style;
      try {
        style = getComputedStyle(el);
      } catch (e) {
        continue;
      }
      if (style.overflowY !== "auto" && style.overflowY !== "scroll") continue;
      var area = el.clientWidth * el.clientHeight;
      if (area > bestArea) {
        bestArea = area;
        best = el;
      }
    }
    return best || document.scrollingElement || document.documentElement;
  }

  function scrollChatToBottom() {
    // Gemini has changed its nesting a few times. The largest scrollable node is
    // not always the conversation (the sidebar can win), so scroll every large
    // pane in the main/right part of the window as well as the document itself.
    try {
      var scrollers = [chatScroller(), document.scrollingElement, document.documentElement];
      var all = document.querySelectorAll("div,main,section");
      for (var i = 0; i < all.length; i++) {
        var el = all[i];
        if (el.scrollHeight <= el.clientHeight + 40) continue;
        var r = el.getBoundingClientRect();
        if (r.width < window.innerWidth * 0.4 || r.right < window.innerWidth * 0.55) continue;
        var style = getComputedStyle(el);
        if (style.overflowY === "auto" || style.overflowY === "scroll") scrollers.push(el);
      }
      uniqueElements(scrollers).forEach(function (sc) {
        if (sc) sc.scrollTop = sc.scrollHeight;
      });
      window.scrollTo(0, Math.max(document.body.scrollHeight, document.documentElement.scrollHeight));
    } catch (e) {}
  }

  function looksLikeLargeImage(img) {
    try {
      var r = img.getBoundingClientRect();
      return (
        img.naturalWidth >= 256 ||
        img.naturalHeight >= 256 ||
        Number(img.getAttribute("width")) >= 256 ||
        Number(img.getAttribute("height")) >= 256 ||
        r.width >= 220 ||
        r.height >= 220
      );
    } catch (e) {
      return false;
    }
  }

  // Gemini's composer is fixed over the conversation. At scrollTop=max the
  // bottom of a result can therefore still sit behind it. Put a real spacer
  // after the result's conversation item so the pane has enough extra scroll
  // range to lift the whole image above the composer.
  function reserveGeminiResultSpace(img) {
    if (!IS_GEMINI || !img) return;
    try {
      var sc = img.parentElement;
      while (sc && sc !== document.body && sc !== document.documentElement) {
        var style = getComputedStyle(sc);
        if (
          sc.scrollHeight > sc.clientHeight + 20 &&
          (style.overflowY === "auto" || style.overflowY === "scroll")
        ) {
          break;
        }
        sc = sc.parentElement;
      }
      if (!sc || sc === document.body || sc === document.documentElement) sc = chatScroller();
      if (!sc || !sc.appendChild) return;

      var item = img;
      while (item.parentElement && item.parentElement !== sc) item = item.parentElement;

      var spacer = document.getElementById("iai-gemini-bottom-spacer");
      if (!spacer) {
        spacer = document.createElement("div");
        spacer.id = "iai-gemini-bottom-spacer";
        spacer.setAttribute("aria-hidden", "true");
      }
      var ce = composer();
      var composerTop = ce ? ce.getBoundingClientRect().top : window.innerHeight - 140;
      var reserve = Math.max(160, Math.ceil(window.innerHeight - composerTop + 48));
      spacer.style.cssText =
        "display:block!important;width:1px!important;min-height:" +
        reserve +
        "px!important;height:" +
        reserve +
        "px!important;flex:0 0 " +
        reserve +
        "px!important;pointer-events:none!important;";
      if (item.parentElement === sc) item.insertAdjacentElement("afterend", spacer);
      else sc.appendChild(spacer);
      sc.scrollTop = sc.scrollHeight;
    } catch (e) {}
  }

  function status(id, message) {
    chrome.runtime.sendMessage({ type: "status", id: id, message: message });
  }

  function fail(id, message) {
    chrome.runtime.sendMessage({ type: "status", id: id, message: message });
    chrome.runtime.sendMessage({ type: "error", id: id, message: message });
  }

  function sendMessage(msg) {
    return new Promise(function (resolve) {
      try {
        chrome.runtime.sendMessage(msg, function (resp) {
          resolve(resp || { ok: false, error: chrome.runtime.lastError && chrome.runtime.lastError.message });
        });
      } catch (e) {
        resolve({ ok: false, error: String(e) });
      }
    });
  }

  function sleep(ms) {
    return new Promise(function (resolve) {
      setTimeout(resolve, ms);
    });
  }

  async function waitForComposerReady(timeoutMs) {
    var deadline = Date.now() + (timeoutMs || 10000);
    while (Date.now() < deadline) {
      if (composer()) return true;
      await sleep(250);
    }
    return false;
  }

  function makePngFile(b64) {
    var bin = atob(b64);
    var bytes = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return new File([bytes], "iai.png", { type: "image/png" });
  }

  function dispatchPasteImage(file) {
    var ce = composer();
    if (!ce) return false;
    ce.focus();
    try {
      var dt = new DataTransfer();
      dt.items.add(file);
      ce.dispatchEvent(new ClipboardEvent("paste", { bubbles: true, cancelable: true, clipboardData: dt }));
      return true;
    } catch (e) {
      try {
        var ev = new Event("paste", { bubbles: true, cancelable: true });
        Object.defineProperty(ev, "clipboardData", { value: { files: [file], items: [file], types: ["Files"] } });
        ce.dispatchEvent(ev);
        return true;
      } catch (_) {
        return false;
      }
    }
  }

  function attachImage(b64) {
    try {
      var file = makePngFile(b64);
      var inputs = deepQuery('input[type=file]');
      inputs.forEach(function (inp) {
        try {
          var dt = new DataTransfer();
          dt.items.add(file);
          inp.files = dt.files;
          inp.dispatchEvent(new Event("input", { bubbles: true }));
          inp.dispatchEvent(new Event("change", { bubbles: true }));
        } catch (e) {}
      });
      var pasted = dispatchPasteImage(file);
      // Also try a synthetic drag-drop onto the composer (some builds use that).
      var targets = [];
      var ce = composer();
      if (ce) targets.push(ce);
      if (document.body) targets.push(document.body);
      targets.forEach(function (t) {
        try {
          var dt = new DataTransfer();
          dt.items.add(file);
          ["dragenter", "dragover", "drop"].forEach(function (type) {
            t.dispatchEvent(new DragEvent(type, { bubbles: true, cancelable: true, dataTransfer: dt }));
          });
        } catch (e) {}
      });
      return "inputs=" + inputs.length + ", paste_event=" + pasted;
    } catch (e) {
      return "attach_err:" + e;
    }
  }

  function fillPrompt(prompt) {
    var ce = composer();
    if (!ce) return false;
    ce.focus();
    if (prompt) {
      try {
        var dt = new DataTransfer();
        dt.setData("text/plain", prompt);
        ce.dispatchEvent(new ClipboardEvent("paste", { bubbles: true, cancelable: true, clipboardData: dt }));
      } catch (e) {}
      if (ce.tagName === "TEXTAREA") {
        ce.value = prompt;
      } else {
        try {
          document.execCommand("selectAll", false, null);
          document.execCommand("insertText", false, prompt);
        } catch (e) {
          ce.textContent = prompt;
        }
      }
      ce.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: prompt }));
    }
    return true;
  }

  function promptText() {
    var ce = composer();
    if (!ce) return "";
    return (ce.tagName === "TEXTAREA" ? ce.value : ce.innerText || ce.textContent || "").replace(/\s+/g, " ").trim();
  }

  function promptLooksFilled(prompt) {
    var want = String(prompt || "").replace(/\s+/g, " ").trim();
    var got = promptText();
    if (!want) return true;
    if (got.indexOf(want) >= 0) return true;
    return got.indexOf(want.slice(0, Math.min(80, want.length))) >= 0;
  }

  async function pasteClipboardIntoComposer(id, label) {
    if (!composer()) return { ok: false, error: "missing composer" };
    await focusComposerReal(id);
    await sleep(120);
    var r = await sendMessage({ type: "realPaste", id: id });
    if (!r || !r.ok) {
      status(id, label + " bang Ctrl+V chua duoc (" + (r && r.error ? r.error : "unknown") + ") - dung cach du phong, van tiep tuc");
      return r || { ok: false, error: "unknown" };
    }
    return r;
  }

  async function clickSendReal(id) {
    var p = sendButtonCenter();
    if (!p) return { ok: false, error: "missing send button" };
    var r = await sendMessage({ type: "realClick", id: id, x: p.x, y: p.y });
    if (r && r.ok) return r;
    return { ok: false, error: r && r.error ? r.error : "unknown" };
  }

  // Put the text caret in the composer with a trusted click (DOM focus() alone
  // often doesn't stick on Gemini's custom composer, so insertText lands nowhere).
  async function focusComposerReal(id) {
    focusComposer();
    // ChatGPT's composer can move while the tab is being focused (attachment
    // chips and the previous generated image both affect the layout). A trusted
    // coordinate click calculated before that move can consequently land on the
    // previous result and open its lightbox. DOM focus is sufficient for
    // debugger paste/insertText on ChatGPT; retain the trusted click only for
    // Gemini's custom editor, where focus() alone is not reliable.
    if (!IS_CHATGPT) {
      var p = composerCenter();
      if (p) await sendMessage({ type: "realClick", id: id, x: p.x, y: p.y });
    }
    focusComposer();
    await sleep(150);
  }

  async function pressEnterReal(id) {
    var r = await sendMessage({ type: "realEnter", id: id });
    if (r && r.ok) return r;
    return { ok: false, error: r && r.error ? r.error : "unknown" };
  }

  async function typePromptReal(id, prompt) {
    if (!focusComposer()) return { ok: false, error: "missing composer" };
    await sleep(120);
    var r = await sendMessage({ type: "realType", id: id, text: prompt });
    if (r && r.ok) return r;
    return { ok: false, error: r && r.error ? r.error : "unknown" };
  }

  function stripDataUrl(s) {
    var i = s.indexOf("base64,");
    return i >= 0 ? s.slice(i + 7) : s;
  }

  // Gemini serves the on-screen result downscaled via a googleusercontent size
  // suffix (…=w512-h512 / …=s512-c). Rewriting it to =s0 asks for the ORIGINAL
  // full-resolution file, which the background can then fetch. Returns null when
  // the URL isn't a googleusercontent one or already has no size suffix.
  function stripSizeToFull(u) {
    if (!u || !/googleusercontent\.com/.test(u)) return null;
    var full = u.replace(/=[-\w]+$/i, "=s0");
    return full !== u ? full : null;
  }

  // Every URL that might hold a higher-quality version of this result, best first:
  // the full-res googleusercontent rewrite, each srcset entry, and the element src.
  // Deliberately does NOT walk ancestors for <a> download links: on a chat page an
  // ancestor spans several turns, so that grabbed an OLDER result's download URL.
  // Only THIS element's own sources are used, so a fetch can never return a
  // different image than the one detected.
  function candidateUrls(img) {
    var urls = [];
    function push(u) {
      if (u && urls.indexOf(u) < 0) urls.push(u);
    }
    var src = img.currentSrc || img.src || "";
    push(stripSizeToFull(src));
    try {
      var ss = img.getAttribute("srcset");
      if (ss) {
        ss.split(",")
          .map(function (s) {
            return s.trim().split(/\s+/)[0];
          })
          .filter(Boolean)
          .forEach(function (u) {
            push(stripSizeToFull(u));
            push(u);
          });
      }
    } catch (e) {}
    push(src);
    return urls;
  }

  // Read one URL to pure base64: content-script fetch (blob:/same-origin) →
  // background fetch (cross-origin googleusercontent/oaiusercontent, covered by
  // host_permissions, which the content script's fetch can't reach under CORS) →
  // canvas draw of the element (last resort; works only when not tainted).
  function readUrlToBase64(url, imgForCanvas, cb) {
    if (!url) {
      cb(null);
      return;
    }
    fetch(url, { credentials: "include" })
      .then(function (r) {
        // Google's gg-dl download URL serves the image as application/octet-stream,
        // so DON'T gate on content-type — accept any 2xx body and let the app's
        // image decoder reject a non-image (it validates before placing).
        if (!r.ok) throw new Error("http " + r.status);
        return r.blob();
      })
      .then(function (b) {
        var fr = new FileReader();
        fr.onloadend = function () {
          cb(stripDataUrl(fr.result));
        };
        fr.readAsDataURL(b);
      })
      .catch(function () {
        chrome.runtime.sendMessage({ type: "fetchImage", src: url }, function (resp) {
          if (resp && resp.image) {
            cb(resp.image);
            return;
          }
          try {
            var c = document.createElement("canvas");
            c.width = imgForCanvas.naturalWidth;
            c.height = imgForCanvas.naturalHeight;
            c.getContext("2d").drawImage(imgForCanvas, 0, 0);
            cb(stripDataUrl(c.toDataURL("image/png")));
          } catch (e) {
            cb(null);
          }
        });
      });
  }

  // Try every candidate URL in order; return the first that yields image bytes
  // (full quality — the =s0 rewrite and srcset entries are tried before the
  // downscaled on-screen src).
  function imgToBase64Multi(img, cb) {
    var urls = candidateUrls(img);
    var i = 0;
    (function next() {
      if (i >= urls.length) {
        cb(null);
        return;
      }
      readUrlToBase64(urls[i++], img, function (b64) {
        if (b64) cb(b64);
        else next();
      });
    })();
  }

  function elementCenter(el) {
    if (!el) return null;
    try {
      el.scrollIntoView({ block: "center", inline: "center" });
    } catch (e) {}
    var r = el.getBoundingClientRect();
    if (!r || r.width <= 0 || r.height <= 0) return null;
    return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  }

  function controlLabel(el) {
    return normText(
      buttonText(el) +
        " " +
        (el.getAttribute("aria-label") || "") +
        " " +
        (el.getAttribute("mattooltip") || "") +
        " " +
        (el.getAttribute("title") || "")
    );
  }

  // The page's own "Copy image" control (Gemini "Sao chép hình ảnh", ChatGPT
  // "Copy image"). Clicking it makes the SITE put the full-resolution original on
  // the system clipboard — which iAi then reads natively at full quality.
  function findCopyControl() {
    var btns = deepQuery('button,[role="button"],[aria-label],[mattooltip]');
    for (var i = 0; i < btns.length; i++) {
      if (!isVisible(btns[i])) continue;
      if (/sao chep (hinh anh|anh|hinh)|copy image|copy photo|copy picture/.test(controlLabel(btns[i]))) {
        return btns[i];
      }
    }
    return null;
  }

  // The page's own "Download (full-size) image" control (Gemini "Tải hình ảnh có
  // kích thước đầy đủ xuống", ChatGPT "Download"). Used ONLY as a readiness signal
  // — its presence means the final image + toolbar have rendered, so grabbing now
  // reads the FINAL full-res src rather than a preview.
  function findDownloadControl() {
    var btns = deepQuery('button,[role="button"],[aria-label],[mattooltip],a[download]');
    for (var i = 0; i < btns.length; i++) {
      if (!isVisible(btns[i])) continue;
      if (btns[i].hasAttribute && btns[i].hasAttribute("download")) return btns[i];
      if (
        /tai hinh anh|tai xuong|tai ve|kich thuoc day du|download|save image|luu (anh|hinh)/.test(
          controlLabel(btns[i])
        )
      ) {
        return btns[i];
      }
    }
    return null;
  }

  // A "more options" / overflow (⋮) button near the result — the copy control is
  // sometimes tucked inside it. Returns the closest such button to the result img.
  function findMoreButton(img) {
    var n = img;
    for (var h = 0; h < 9 && n; h++) {
      try {
        var btns = n.querySelectorAll('button,[role="button"]');
        for (var i = 0; i < btns.length; i++) {
          if (!isVisible(btns[i])) continue;
          if (/more|tuy chon|tùy chọn|options|khac|khác|overflow|menu/.test(controlLabel(btns[i]))) {
            return btns[i];
          }
        }
      } catch (e) {}
      n = n.parentElement;
    }
    return null;
  }

  // Full-quality fallback: drive the page's "Copy image" control so the SITE puts
  // the original PNG on the system clipboard, which iAi then reads natively. The
  // clicks are HEADLESS — focus is emulated via the debugger (not by raising the
  // window), so the browser never pops in front of the user, yet the site's
  // clipboard write (which needs document focus) still succeeds. iAi polls the
  // clipboard and guards against a stale one by hashing. Resolves cb(true) once
  // handed off to iAi, cb(false) if no copy control could be found/opened.
  async function grabViaCopy(img, id, cb) {
    // Emulate focus first so the composer/toolbar behave as if the tab is active.
    await sendMessage({ type: "emulateFocus", id: id, enabled: true });
    var btn = findCopyControl();
    if (!btn) {
      var more = findMoreButton(img);
      if (more) {
        var mp = elementCenter(more);
        if (mp) {
          await sendMessage({ type: "clickNoFocus", id: id, x: mp.x, y: mp.y });
          await sleep(500);
          btn = findCopyControl();
        }
      }
    }
    if (!btn) {
      cb(false);
      return;
    }
    var p = elementCenter(btn);
    if (!p) {
      cb(false);
      return;
    }
    status(id, "Da tim thay nut Sao chep anh, dang bam de lay ban goc...");
    await sendMessage({ type: "clickNoFocus", id: id, x: p.x, y: p.y });
    await sleep(700); // brief settle; iAi then polls the clipboard for a few seconds
    status(id, "Da bam Copy, dang cho anh vao IAI...");
    chrome.runtime.sendMessage({ type: "result_clipboard", id: id });
    cb(true);
  }

  // Compact one-glance description of a URL for the failure diagnostic.
  function briefUrl(u) {
    if (!u) return "(none)";
    if (/^data:/.test(u)) return "data[" + u.length + "b]";
    if (/^blob:/.test(u)) return "blob:" + u.slice(5, 50);
    try {
      var a = new URL(u);
      return a.host + a.pathname.slice(0, 34);
    } catch (e) {
      return u.slice(0, 60);
    }
  }

  // When BOTH grab paths fail, dump the real page structure to the app's status
  // log so the owner can screenshot it: the exact result-image src and the labels
  // of the buttons around it. This is the ground truth needed to fix detection.
  function dumpDiag(img, id) {
    try {
      status(
        id,
        "DIAG " +
          SITE +
          " img=" +
          img.tagName +
          " " +
          briefUrl(img.currentSrc || img.src) +
          " nat=" +
          img.naturalWidth +
          "x" +
          img.naturalHeight +
          " srcset=" +
          (img.getAttribute("srcset") ? "yes" : "no") +
          " urls=" +
          candidateUrls(img).length
      );
    } catch (e) {}
    try {
      var labels = [];
      var n = img;
      for (var h = 0; h < 8 && n; h++) {
        var bs = n.querySelectorAll('button,[role="button"],[aria-label],[mattooltip],a[download],a[href]');
        for (var i = 0; i < bs.length && labels.length < 16; i++) {
          var l = controlLabel(bs[i]).trim().replace(/\s+/g, " ").slice(0, 22);
          if (l && labels.indexOf(l) < 0) labels.push(l);
        }
        if (labels.length >= 16) break;
        n = n.parentElement;
      }
      status(id, "DIAG nut: " + (labels.join(" | ") || "(khong thay nut nao quanh anh)"));
    } catch (e) {}
  }

  function armGrab(id, timeoutMs, submittedPrompt) {
    var started = Date.now();
    var beforeElements = Array.prototype.slice.call(document.querySelectorAll("img"));
    var before = new Set(
      Array.prototype.map.call(beforeElements, function (i) {
        return i.currentSrc || i.src;
      })
    );
    var done = false;
    var submitted = false;
    var obs;
    var timer;
    var poll;
    var scanTimer;
    var nudged = [];
    var trustedScrollPending = false;

    function trustedGeminiScroll() {
      if (!IS_GEMINI || trustedScrollPending || done) return;
      trustedScrollPending = true;
      // Aim above the fixed composer, inside Gemini's conversation pane.
      sendMessage({
        type: "realScroll",
        id: id,
        x: Math.round(window.innerWidth * 0.68),
        y: Math.round(window.innerHeight * 0.42),
      }).then(function () {
        trustedScrollPending = false;
      });
    }
    function cleanup() {
      try {
        obs && obs.disconnect();
      } catch (e) {}
      try {
        timer && clearTimeout(timer);
      } catch (e) {}
      try {
        poll && clearInterval(poll);
      } catch (e) {}
      try {
        scanTimer && clearTimeout(scanTimer);
      } catch (e) {}
      scanTimer = null;
      delete activeJobs[id];
    }
    activeJobs[id] = {
      cancel: function () {
        done = true;
        cleanup();
      },
      markSubmitted: function () {
        if (done || submitted) return;
        submitted = true;
        started = Date.now();
        scheduleScan();
      },
    };
    function finish(img) {
      if (done) return;
      if (IS_GEMINI) {
        reserveGeminiResultSpace(img);
        try {
          img.scrollIntoView({ block: "end", inline: "nearest" });
        } catch (e) {}
        scrollChatToBottom();
      }
      done = true;
      cleanup();
      status(id, "Da thay anh ket qua, cho thanh nut hien roi tai ban goc...");

      function doGrab() {
        // 1) Best path: download the ORIGINAL bytes via background fetch (bypasses
        //    CORS; content-type no longer gated so gg-dl octet-stream works too).
        //    Headless, full quality.
        imgToBase64Multi(img, function (b64) {
          if (b64) {
            chrome.runtime.sendMessage({ type: "result", id: id, image: b64 });
            return;
          }
          // 2) Fallback the user asked for: click the page's "Copy image" control so
          //    the SITE puts the full-res original on the clipboard; iAi reads it.
          status(id, "Tai truc tiep khong duoc, thu nut Sao chep anh tren trang...");
          grabViaCopy(img, id, function (handed) {
            if (!handed) {
              dumpDiag(img, id);
              fail(
                id,
                "Khong lay duoc anh ket qua (khong tai duoc & khong thay nut Sao chep anh tren " +
                  SITE +
                  ")"
              );
            }
            // handed === true: iAi reads the clipboard and reports success/failure.
          });
        });
      }

      // The FINAL full-res src and the action toolbar (Copy / Download full-size)
      // appear a beat after the image first renders. Grabbing immediately used to
      // read a preview src (fetch failed) and miss the not-yet-rendered Copy
      // button. Wait for the toolbar — or a 6s cap — before grabbing.
      var waited = 0;
      (function waitControls() {
        // Min ~500ms settle even if a prior turn's toolbar already exists, so a
        // second result isn't grabbed before its own final src lands; up to 6s for
        // the toolbar to appear on the first result.
        var ready = findCopyControl() || findDownloadControl();
        if ((ready && waited >= 500) || waited >= 6000) {
          doGrab();
        } else {
          waited += 250;
          setTimeout(waitControls, 250);
        }
      })();
    }
    // Force a lazy image (below the viewport) to load instead of waiting for the
    // user to scroll it into view; its `load` event re-runs the scan.
    function nudgeLoad(img) {
      if (nudged.indexOf(img) >= 0) return;
      nudged.push(img);
      try {
        img.loading = "eager";
      } catch (e) {}
      try {
        img.setAttribute("loading", "eager");
      } catch (e) {}
      try {
        img.scrollIntoView({ block: IS_GEMINI ? "end" : "center", inline: "nearest" });
      } catch (e) {}
      if (IS_GEMINI) scrollChatToBottom();
      img.addEventListener("load", scheduleScan, { once: true });
    }
    function scan() {
      if (done || !submitted) return;
      // A Gemini result can remain lazy/unloaded until visible. Nudge every new
      // non-composer image first; waiting for result controls before scrolling
      // creates a deadlock on builds that add those controls only after loading.
      if (IS_GEMINI) {
        var cb = composerBox();
        Array.prototype.forEach.call(document.querySelectorAll("img"), function (img) {
          // Do not treat a src/srcset swap on an existing lazy image as a new
          // result. Scrolling can load an older Gemini result and change its src.
          var isNew = beforeElements.indexOf(img) < 0;
          if (isNew && !(cb && cb.contains(img)) && looksLikeLargeImage(img)) {
            reserveGeminiResultSpace(img);
            nudgeLoad(img);
          }
        });
      }
      var candidates = Array.prototype.filter.call(document.querySelectorAll("img"), function (i) {
        var src = i.currentSrc || i.src || "";
        // A chat UI can virtualize an older turn while we scroll: the same old
        // result then comes back as a brand-new <img> node. Requiring both a
        // new node and a source that was not present at arm time keeps that
        // re-created historical image outside this job's result set. A genuine
        // result may replace its preview src, but its node is still new.
        var isNew = beforeElements.indexOf(i) < 0 && !!src && !before.has(src);
        return isNew && isResultCandidate(i);
      });
      candidates.sort(function (a, b) {
        return imagePageBottom(a) - imagePageBottom(b);
      });
      for (var i = candidates.length - 1; i >= 0; i--) {
        if (isLoadedResult(candidates[i])) {
          finish(candidates[i]);
          return;
        }
      }
      for (var j = 0; j < candidates.length; j++) nudgeLoad(candidates[j]);
    }
    // Generation causes mutation storms and scan walks ancestors per image —
    // coalesce bursts into one scan per 250ms.
    function scheduleScan() {
      if (done || !submitted || scanTimer) return;
      scanTimer = setTimeout(function () {
        scanTimer = null;
        scan();
      }, 250);
    }
    obs = new MutationObserver(scheduleScan);
    obs.observe(document.body, {
      subtree: true,
      childList: true,
      attributes: true,
      attributeFilter: ["src", "srcset"],
    });
    // Gemini renders the result below the viewport; keep the chat pinned to the
    // bottom so it loads without the user wheeling down. ChatGPT is different:
    // keep the viewport still while generation is running, and wait until the
    // new result image looks stable before nudging any lazy image.
    poll = setInterval(function () {
      if (done) return;
      // Never scan or scroll while the command is still sitting in the composer.
      // This is the hard job boundary that prevents an older image being returned.
      if (!submitted) {
        if (!promptLooksFilled(submittedPrompt)) {
          submitted = true;
          started = Date.now();
        } else {
          return;
        }
      }
      if (IS_GEMINI) {
        scrollChatToBottom();
        trustedGeminiScroll();
      }
      scan();
    }, 1200);
    timer = setTimeout(function () {
      if (!done) {
        done = true;
        cleanup();
        fail(id, "Het thoi gian cho anh ket qua (timeout)");
      }
    }, timeoutMs || 180000);
  }
  async function runEdit(msg) {
    if (activeJobs[msg.id]) activeJobs[msg.id].cancel();
    var cancelled = false;
    activeJobs[msg.id] = {
      cancel: function () {
        cancelled = true;
        delete activeJobs[msg.id];
      },
    };
    function stopIfCancelled() {
      return cancelled || !activeJobs[msg.id];
    }

    // IAI already puts the image on the OS clipboard natively when it sends the
    // job (msg.clipboard) — far more reliable than the offscreen-document copy,
    // which is kept only as the fallback for older IAI builds.
    var clipReady = !!msg.clipboard;
    if (clipReady) {
      status(msg.id, "Buoc 1/3: IAI da dat anh vao clipboard he thong...");
    } else {
      status(msg.id, "Buoc 1/3: dang copy anh vao clipboard...");
      var clipImage = await sendMessage({ type: "clipboardImage", image: msg.image });
      if (stopIfCancelled()) return;
      clipReady = !!(clipImage && clipImage.ok);
      if (!clipReady) {
        status(msg.id, "Extension khong tu copy duoc anh - chuyen sang gan anh truc tiep (van binh thuong)");
      }
    }

    if (!(await waitForComposerReady(12000))) {
      fail(msg.id, "Khong tim thay o nhap ChatGPT/Gemini - hay reload tab " + SITE + " roi gui lai");
      delete activeJobs[msg.id];
      return;
    }

    var beforeAttach = new Set(
      composerImages().map(function (img) {
        return imageKey(img);
      })
    );
    var pastedImage = false;
    if (clipReady) {
      status(msg.id, "Buoc 1/3: dang Ctrl+V anh vao " + SITE + "...");
      var pasteResult = await pasteClipboardIntoComposer(msg.id, "Dan anh");
      pastedImage = !!(pasteResult && pasteResult.ok);
      await sleep(IS_CHATGPT ? 3200 : 2200);
    }
    if (stopIfCancelled()) return;

    var attached = hasNewAttachment(beforeAttach);
    if (!attached && !pastedImage) {
      // Only fall back to the DOM attach when the native Ctrl+V paste did NOT
      // happen (older IAI, or the paste dispatch failed). The attachment detector
      // is unreliable — Gemini often renders the chip without a matching <img> in
      // the composer box — so running this backup AFTER a successful paste
      // attached the image TWICE. Trust the paste; if it truly didn't attach,
      // Gemini's result shows it and the user can retry.
      attachImage(msg.image);
      await sleep(1500);
      attached = hasNewAttachment(beforeAttach);
    }
    if (stopIfCancelled()) return;
    // The preview detector is unreliable, so DON'T abort on it. The image is on the
    // clipboard and was pasted, so continue to the prompt regardless — if the image
    // really didn't attach, Gemini's result will show it and the user can retry.
    status(
      msg.id,
      attached
        ? "Da dan anh, chuyen sang prompt..."
        : "Da gui anh (detector khong chac), van tiep tuc dan prompt..."
    );

    // Prompt: a trusted click sets the caret in the composer, then insert the text
    // via the debugger (most reliable). Clipboard-paste competes with the image on
    // the clipboard, so it is only a fallback here.
    status(msg.id, "Buoc 2/3: dang nhap prompt vao " + SITE + "...");
    await focusComposerReal(msg.id);
    await typePromptReal(msg.id, msg.prompt);
    await sleep(450);
    if (stopIfCancelled()) return;
    if (!promptLooksFilled(msg.prompt)) {
      status(msg.id, "insertText chua vao, thu Ctrl+V prompt...");
      var clipText = await sendMessage({ type: "clipboardText", text: msg.prompt });
      if (clipText && clipText.ok) {
        await focusComposerReal(msg.id);
        await pasteClipboardIntoComposer(msg.id, "Dan prompt");
        await sleep(450);
      }
    }
    if (!promptLooksFilled(msg.prompt)) {
      status(msg.id, "Thu dien prompt bang DOM...");
      fillPrompt(msg.prompt);
      await sleep(350);
    }
    if (!promptLooksFilled(msg.prompt)) {
      fail(msg.id, "Prompt chua vao duoc o chat " + SITE + " - thu reload tab " + SITE + " roi gui lai");
      delete activeJobs[msg.id];
      return;
    }

    // Send: arm the result observer, then keep trying until the prompt leaves the
    // composer. Gemini DISABLES Send while the attachment is still uploading, so a
    // single Enter often lands in that window and does nothing — wait for an
    // enabled Send button between attempts and retry (Enter first, real click as
    // backup) for up to ~45s on Gemini, longer on ChatGPT where the web UI can
    // keep Send locked while it preprocesses an uploaded image. The loop also
    // stops as soon as the job finishes
    // (result grabbed or cancelled), since armGrab owns activeJobs[msg.id] now.
    status(msg.id, "Buoc 3/3: da co anh + prompt, dang gui...");
    armGrab(msg.id, 180000, msg.prompt);
    await sleep(300);
    var sendDeadline = Date.now() + (IS_CHATGPT ? 120000 : 45000);
    var sent = false;
    var waitNoted = false;
    var lastBlindEnter = 0;
    while (Date.now() < sendDeadline && !stopIfCancelled()) {
      var sendBtn = findSendButton(true);
      if (!sendBtn || isDisabled(sendBtn)) {
        if (!waitNoted) {
          waitNoted = true;
          status(
            msg.id,
            sendBtn
              ? "Nut Gui dang khoa (" + SITE + " con xu ly anh) - cho mo khoa..."
              : "Chua thay nut Gui cua " + SITE + " - thu Enter du phong..."
          );
        }
        if (IS_CHATGPT && !sendBtn && Date.now() - lastBlindEnter > 2500) {
          lastBlindEnter = Date.now();
          await focusComposerReal(msg.id);
          await pressEnterReal(msg.id);
          await sleep(700);
          if (stopIfCancelled()) return;
          if (!promptLooksFilled(msg.prompt)) {
            sent = true;
            break;
          }
        }
        await sleep(500);
        continue;
      }
      await focusComposerReal(msg.id);
      await pressEnterReal(msg.id);
      await sleep(700);
      if (stopIfCancelled()) return;
      if (!promptLooksFilled(msg.prompt)) {
        sent = true;
        break;
      }
      var clicked = await clickSendReal(msg.id);
      if (!(clicked && clicked.ok)) clickSend();
      await sleep(900);
      if (!promptLooksFilled(msg.prompt)) {
        sent = true;
        break;
      }
      status(msg.id, "Chua gui duoc, thu lai...");
    }
    if (stopIfCancelled()) return;
    if (sent) {
      if (activeJobs[msg.id] && activeJobs[msg.id].markSubmitted) {
        activeJobs[msg.id].markSubmitted();
      }
      status(
        msg.id,
        IS_CHATGPT
          ? "Da gui sang ChatGPT, dang cho anh tao xong roi moi tai ve..."
          : "Da gui sang Gemini, dang cho ket qua..."
      );
    } else {
      status(
        msg.id,
        "Chua tu gui duoc sau nhieu lan thu - hay bam Send (mui ten) tren " + SITE + "; extension van dang cho ket qua."
      );
    }
  }
  chrome.runtime.onMessage.addListener(function (msg) {
    if (!msg) return;
    if (msg.type === "cancel") {
      var job = activeJobs[msg.id];
      if (job) job.cancel();
      status(msg.id, "Da huy lenh trong tab " + SITE);
      return;
    }
    if (msg.type !== "edit") return;
    runEdit(msg).catch(function (e) {
      fail(msg.id, "Loi chay content script " + SITE + ": " + e);
    });
  });
  chrome.runtime.sendMessage({ type: "status", message: "Content script " + SITE + " sẵn sàng" });
})();





