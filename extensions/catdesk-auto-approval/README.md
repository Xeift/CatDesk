# CatDesk Auto Approval

A small Chrome extension that auto click ChatGPT tool approval UI.

## Behavior

- Click the extension icon to toggle enabled / disabled.
- Enabled mode:
  - Watches `https://chatgpt.com/*` pages.
  - Detects ChatGPT tool approval cards.
  - Highlights only the primary approval button using the CatDesk widget grayscale palette.
  - Click the approval button.
- Disabled mode:
  - Stops watching the page.
  - Removes the approval highlight and overlay label.

## Selection strategy

The extension does not rely on the changing approval button text.

Instead, it finds a negative decision button such as `Deny`, `Cancel`, `Reject`, `Decline`, or `No`; finds the surrounding approval card; then selects the same-row button to the right of the negative button as the primary approval candidate.

The candidate is only highlighted and clicked when the surrounding card contains tool approval signals such as:

- `Using tools comes with risks`
- `Sharing data includes`
- `provided content`
- `create or overwrite`
- `sensitive`
- `workspace`
- `file`

## Install locally

1. Open Chrome.
2. Go to `chrome://extensions`.
3. Enable Developer mode.
4. Click `Load unpacked`.
5. Select this folder:

```text
extensions/catdesk-auto-approval
```
