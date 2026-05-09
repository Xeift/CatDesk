const STORAGE_KEY = "enabled";
const CHATGPT_URL_PATTERN = /^https:\/\/chatgpt\.com\//;

async function getEnabled() {
  const result = await chrome.storage.local.get({ [STORAGE_KEY]: false });
  return Boolean(result[STORAGE_KEY]);
}

async function setEnabled(enabled) {
  await chrome.storage.local.set({ [STORAGE_KEY]: Boolean(enabled) });
  await updateAction(enabled);
}

async function updateAction(enabled) {
  await chrome.action.setBadgeText({ text: enabled ? "ON" : "OFF" });
  await chrome.action.setBadgeBackgroundColor({ color: enabled ? "#383838" : "#212121" });
  await chrome.action.setTitle({
    title: enabled
      ? "CatDesk Auto Approval: enabled"
      : "CatDesk Auto Approval: disabled",
  });
}

async function ensureContentScript(tabId) {
  await chrome.scripting.executeScript({
    target: { tabId },
    files: ["content.js"],
  });
}

async function notifyTab(tabId, enabled) {
  try {
    await chrome.tabs.sendMessage(tabId, {
      type: "CHATGPT_APPROVAL_HIGHLIGHTER_SET_ENABLED",
      enabled,
    });
  } catch (error) {
    console.warn("Unable to notify tab", error);
  }
}

chrome.runtime.onInstalled.addListener(async () => {
  const enabled = await getEnabled();
  await updateAction(enabled);
});

chrome.runtime.onStartup.addListener(async () => {
  const enabled = await getEnabled();
  await updateAction(enabled);
});

chrome.action.onClicked.addListener(async (tab) => {
  const nextEnabled = !(await getEnabled());
  await setEnabled(nextEnabled);

  if (!tab?.id || !tab.url || !CHATGPT_URL_PATTERN.test(tab.url)) {
    return;
  }

  try {
    await ensureContentScript(tab.id);
    await notifyTab(tab.id, nextEnabled);
  } catch (error) {
    console.error("Failed to inject or notify content script", error);
  }
});
