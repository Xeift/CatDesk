(() => {
  if (globalThis.__chatgptApprovalHighlighter?.version === "0.2.2") {
    globalThis.__chatgptApprovalHighlighter.reinitialize();
    return;
  }

  const STORAGE_KEY = "enabled";
  const OVERLAY_ATTR = "data-catdesk-auto-approval-overlay";
  const OUTLINED_ATTR = "data-catdesk-auto-approval-outlined";
  const VERSION = "0.2.2";
  const HIGHLIGHT_COLOR = "rgba(255, 255, 255, 0.42)";
  const HIGHLIGHT_BACKGROUND = "rgba(255, 255, 255, 0.14)";
  const HIGHLIGHT_LABEL_BACKGROUND = "rgba(26, 26, 28, 0.96)";
  const HIGHLIGHT_TEXT_COLOR = "#cdcdcd";

  let enabled = false;
  let observer = null;
  let renderTimer = null;
  const outlinedElements = new Set();

  function log(...args) {
    console.info("[ApprovalHighlighter]", ...args);
  }

  function normalizeText(value) {
    return String(value ?? "").replace(/\s+/g, " ").trim();
  }

  function textOf(el) {
    return normalizeText([
      el.innerText,
      el.textContent,
      el.getAttribute("aria-label"),
      el.getAttribute("title"),
      el.value,
    ].filter(Boolean).join(" "));
  }

  function isVisible(el) {
    const rect = el.getBoundingClientRect();
    const style = getComputedStyle(el);

    return (
      rect.width > 0 &&
      rect.height > 0 &&
      style.display !== "none" &&
      style.visibility !== "hidden" &&
      style.opacity !== "0"
    );
  }

  function isEnabled(el) {
    return (
      !el.disabled &&
      el.getAttribute("aria-disabled") !== "true" &&
      !el.closest("[aria-disabled='true']")
    );
  }

  function getButtonLikeElements(root = document) {
    return [...root.querySelectorAll("button, [role='button'], input[type='button'], input[type='submit']")]
      .filter((el) => el instanceof HTMLElement)
      .filter(isVisible)
      .filter(isEnabled);
  }

  function isNegativeButton(el) {
    return /^(deny|cancel|reject|decline|no)(?:\s+\1)*$/i.test(textOf(el));
  }

  function isExcludedControl(el) {
    const text = textOf(el).toLowerCase();
    const aria = normalizeText(el.getAttribute("aria-label")).toLowerCase();

    if (/^(details|learn more|share|copy|edit message|copy message|more actions)$/i.test(text)) return true;
    if (/thought for/i.test(text)) return true;
    if (/share|copy|dictation|send prompt|open conversation options|switch model/.test(aria)) return true;
    if (el.closest("form") || el.closest("[contenteditable='true']")) return true;

    return false;
  }

  function findSmallestUsefulCard(el) {
    let node = el.parentElement;

    for (let depth = 0; node && depth < 14; depth += 1, node = node.parentElement) {
      const text = textOf(node);
      const rect = node.getBoundingClientRect();
      const buttons = node.querySelectorAll("button, [role='button']");

      const notTooHuge = rect.width < window.innerWidth * 0.95 && rect.height < window.innerHeight * 0.85;
      const hasDecisionButtons = buttons.length >= 2;
      const hasToolSignals =
        /using tools comes with risks/i.test(text) ||
        /sharing data includes/i.test(text) ||
        /provided content/i.test(text) ||
        /create or overwrite/i.test(text) ||
        /sensitive/i.test(text) ||
        /workspace/i.test(text) ||
        /api key/i.test(text) ||
        /file/i.test(text);

      if (notTooHuge && hasDecisionButtons && hasToolSignals) return node;
    }

    return null;
  }

  function buttonsOnSameRow(root, referenceButton) {
    const referenceRect = referenceButton.getBoundingClientRect();
    const referenceCenterY = (referenceRect.top + referenceRect.bottom) / 2;

    return getButtonLikeElements(root)
      .filter((button) => {
        const rect = button.getBoundingClientRect();
        const centerY = (rect.top + rect.bottom) / 2;
        return Math.abs(centerY - referenceCenterY) <= 22;
      })
      .sort((a, b) => a.getBoundingClientRect().x - b.getBoundingClientRect().x);
  }

  function scoreCandidate(primary, negative, card, rowButtons) {
    const primaryText = textOf(primary);
    const cardText = textOf(card);
    const primaryRect = primary.getBoundingClientRect();
    const negativeRect = negative.getBoundingClientRect();

    let score = 0;
    const reasons = [];

    score += 120;
    reasons.push("same row as negative button");

    if (primaryRect.x > negativeRect.x) {
      score += 80;
      reasons.push("right of negative button");
    }

    if (rowButtons.length === 2) {
      score += 40;
      reasons.push("two-button decision row");
    }

    if (/using tools comes with risks/i.test(cardText)) {
      score += 70;
      reasons.push("tool risk notice");
    }

    if (/sharing data includes/i.test(cardText)) {
      score += 50;
      reasons.push("sharing data notice");
    }

    if (/create|overwrite|provided content|sensitive|workspace|file|command|run|execute|modify|delete|write|install|push|commit/i.test(cardText)) {
      score += 45;
      reasons.push("state-changing context");
    }

    if (/approve|allow|confirm|continue|accept|ok|yes|run|write|save|create|overwrite|execute|submit/i.test(primaryText)) {
      score += 35;
      reasons.push("affirmative/action-ish label");
    }

    if (primaryRect.width >= 90 && primaryRect.height >= 28) {
      score += 25;
      reasons.push("large primary-like button");
    }

    if (isExcludedControl(primary)) {
      score -= 300;
      reasons.push("excluded control");
    }

    if (isNegativeButton(primary)) {
      score -= 300;
      reasons.push("itself is negative");
    }

    return { el: primary, negative, card, score, reasons, text: primaryText };
  }

  function findCandidates() {
    const allButtons = getButtonLikeElements();
    const negativeButtons = allButtons.filter(isNegativeButton);
    const candidates = [];

    for (const negative of negativeButtons) {
      const card = findSmallestUsefulCard(negative);
      if (!card) continue;

      const rowButtons = buttonsOnSameRow(card, negative);

      for (const button of rowButtons) {
        if (button === negative) continue;
        if (isExcludedControl(button)) continue;

        const buttonRect = button.getBoundingClientRect();
        const negativeRect = negative.getBoundingClientRect();
        if (buttonRect.x <= negativeRect.x) continue;

        candidates.push(scoreCandidate(button, negative, card, rowButtons));
      }
    }

    return candidates.sort((a, b) => b.score - a.score);
  }

  function addOverlay(el, label) {
    const rect = el.getBoundingClientRect();

    el.style.setProperty("outline", `3px solid ${HIGHLIGHT_COLOR}`, "important");
    el.style.setProperty("outline-offset", "3px", "important");
    el.style.setProperty("box-shadow", `0 0 0 6px ${HIGHLIGHT_BACKGROUND}`, "important");
    el.setAttribute(OUTLINED_ATTR, "1");
    outlinedElements.add(el);

    const box = document.createElement("div");
    box.setAttribute(OVERLAY_ATTR, "1");
    box.style.cssText = [
      "position: fixed",
      `left: ${Math.max(0, rect.left)}px`,
      `top: ${Math.max(0, rect.top)}px`,
      `width: ${Math.max(1, rect.width)}px`,
      `height: ${Math.max(1, rect.height)}px`,
      `border: 2px solid ${HIGHLIGHT_COLOR}`,
      `background: ${HIGHLIGHT_BACKGROUND}`,
      "box-sizing: border-box",
      "z-index: 2147483647",
      "pointer-events: none",
      "border-radius: 10px",
    ].join(";");

    const tag = document.createElement("div");
    tag.setAttribute(OVERLAY_ATTR, "1");
    tag.textContent = label;
    tag.style.cssText = [
      "position: fixed",
      `left: ${Math.max(0, rect.left)}px`,
      `top: ${Math.max(0, rect.top - 24)}px`,
      `background: ${HIGHLIGHT_LABEL_BACKGROUND}`,
      `border: 1px solid ${HIGHLIGHT_BACKGROUND}`,
      `color: ${HIGHLIGHT_TEXT_COLOR}`,
      "font: 12px/1.4 system-ui, -apple-system, BlinkMacSystemFont, sans-serif",
      "padding: 2px 6px",
      "border-radius: 6px",
      "z-index: 2147483647",
      "pointer-events: none",
      "max-width: 460px",
      "white-space: nowrap",
      "overflow: hidden",
      "text-overflow: ellipsis",
    ].join(";");

    document.body.appendChild(box);
    document.body.appendChild(tag);
  }

  function clearHighlights() {
    document.querySelectorAll(`[${OVERLAY_ATTR}]`).forEach((el) => el.remove());

    for (const el of outlinedElements) {
      if (!(el instanceof HTMLElement)) continue;
      el.style.removeProperty("outline");
      el.style.removeProperty("outline-offset");
      el.style.removeProperty("box-shadow");
      el.removeAttribute(OUTLINED_ATTR);
    }

    outlinedElements.clear();
  }

  function highlightBestCandidate() {
    clearHighlights();

    const best = findCandidates()[0];
    globalThis.__chatgptApprovalHighlighterLastResult = best ? {
      score: best.score,
      text: best.text,
      reasons: best.reasons,
    } : null;

    if (!best) return null;

    addOverlay(best.el, `APPROVE score=${best.score}`);
    best.el.click();

    return globalThis.__chatgptApprovalHighlighterLastResult;
  }

  function scheduleHighlight() {
    if (!enabled) return;
    clearTimeout(renderTimer);
    renderTimer = setTimeout(highlightBestCandidate, 150);
  }

  function startWatching() {
    if (!observer) {
      observer = new MutationObserver(scheduleHighlight);
      observer.observe(document.body, {
        childList: true,
        subtree: true,
        attributes: true,
        attributeFilter: ["disabled", "aria-disabled", "style", "class"],
      });

      window.addEventListener("scroll", scheduleHighlight, true);
      window.addEventListener("resize", scheduleHighlight, true);
    }

    scheduleHighlight();
  }

  function stopWatching() {
    if (observer) {
      observer.disconnect();
      observer = null;
    }

    window.removeEventListener("scroll", scheduleHighlight, true);
    window.removeEventListener("resize", scheduleHighlight, true);

    clearTimeout(renderTimer);
    renderTimer = null;
    clearHighlights();
  }

  function setEnabled(nextEnabled) {
    enabled = Boolean(nextEnabled);
    log(enabled ? "enabled" : "disabled");

    if (enabled) startWatching();
    else stopWatching();
  }

  const api = {
    version: VERSION,
    setEnabled,
    reinitialize() {
      chrome.storage.local.get({ [STORAGE_KEY]: false }, (result) => setEnabled(result[STORAGE_KEY]));
    },
    highlight: highlightBestCandidate,
    clear: clearHighlights,
    find: findCandidates,
    get enabled() {
      return enabled;
    },
  };

  globalThis.__chatgptApprovalHighlighter = api;

  chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
    if (message?.type !== "CHATGPT_APPROVAL_HIGHLIGHTER_SET_ENABLED") return false;

    setEnabled(message.enabled);
    sendResponse({ ok: true, enabled });
    return false;
  });

  chrome.storage.onChanged.addListener((changes, areaName) => {
    if (areaName !== "local") return;
    if (!changes[STORAGE_KEY]) return;
    setEnabled(changes[STORAGE_KEY].newValue);
  });

  api.reinitialize();
  log("loaded", VERSION);
})();
