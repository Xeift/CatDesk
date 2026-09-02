# CatDesk

[English](README.md) | **繁體中文**

一個開源工具，讓你可以把 ChatGPT Chat 當成本機程式開發代理使用。不需要逆向工程、不需要 API、不需要 Codex，也不需要 Work 模式。只要有 ChatGPT Plus 訂閱即可。

<p align="center">
  <img src="docs/images/catdesk_preview.gif" alt="CatDesk in ChatGPT Web"><br>
  <em>ChatGPT Web 中的 CatDesk</em>
</p>

# 免責聲明

這是一個獨立的開源專案，與 OpenAI 無關，也未獲得 OpenAI 背書。我一開始只是把它做成個人工具，後來決定開源。部分功能仍可能有 bug，並可能造成非預期行為。請自行承擔使用風險。我不對此工具造成的任何損失負責。強烈建議在 VM 或容器內執行。

# 為什麼要用 CatDesk？

和 Antigravity（很會說早安）以及 Claude Code（RIP 5 小時額度 💀）相比，Codex 的每週額度非常大方（而且常常重置用量），這也是我這麼喜歡 OpenAI 的原因。

<p align="center">
  <img src="docs/images/codex_2x_usage.png" alt="Codex reset usage frequently🙏" width="700"><br>
  <em>Codex 經常重置用量🙏</em>
</p>

但如果你在做大型專案，額度仍然會很快用完。

<p align="center">
  <img src="docs/images/no_remaining_usage.png" alt="I used up my Codex quota on the first day after it reset" width="700"><br>
  <em>我的 Codex 額度在重置後第一天就用完了</em>
</p>

然後你得再等 7 天。那剩下這一週要幹嘛？

解法來了：大多數 Plus 使用者每週的 Thinking 訊息額度甚至用不到 10%。

**_那為什麼不用每週 3,000 則訊息來寫程式？_**

這就是 CatDesk 的核心概念！它讓 ChatGPT Web 擁有 `write`、`run_command` 等工具，可以直接修改你電腦上的檔案。

<p align="center">
  <img src="docs/images/thinking_usage_limits.png" alt="ChatGPT reasoning usage limits for GPT-5.5 and GPT-5.6" width="900"><br>
  <em>GPT-5.5：<a href="https://web.archive.org/web/20260519111010/https://help.openai.com/en/articles/11909943-gpt-55-in-chatgpt">每週 3,000 則訊息</a>，GPT-5.6：<a href="https://help.openai.com/en/articles/20001354-gpt-56-in-chatgpt">未知</a>，但我從來沒有撞到上限</em>
</p>

# 原理是什麼？

1. 需要 ChatGPT Plus 或更高級別的訂閱。
2. CatDesk 會在你的電腦上執行成一個本機 MCP server。它可以執行指令和編輯檔案，功能類似 Codex。
3. 你可以透過 Custom Connector，把 ChatGPT Web 連接到 CatDesk。這項功能只提供給 Plus 和 Pro 使用者。
4. 完成！現在 ChatGPT Web 可以控制你的電腦並在上面寫程式。

簡單來說：

```text
ChatGPT Web + CatDesk
= 精簡版 Codex
= 沒有 cron 和其他主動工具的 OpenClaw
```

我以前用 GPT-5.2 測過，效果很差。不過現在 **GPT-5.4 Thinking 在工具呼叫與電腦操作上已經非常強。** 我第一次用 GPT-5.4 測 CatDesk 時，真的被效果嚇到了。GPT-5.5 和 GPT-5.6 又更順，而 GPT-5.6 使用 CatDesk 的能力尤其強，而且速度也非常快。

# ChatGPT Chat + CatDesk、Codex 與 API 的差異（以 Plus 方案為例）

|       | ChatGPT Chat + CatDesk                    | Codex              | OpenAI API       |
| ----- | ----------------------------------------- | ------------------ | ---------------- |
| 用量  | 每週 3,000 則訊息                        | 大方的每週額度     | 按量付費         |
| 優點  | 穩定、不需額外付費，而且額度幾乎無限\* | 穩定且不需額外付費 | 穩定             |
| 缺點  | 沒有原生 Codex 那麼順                    | 很快就會用完       | Token 很貴       |

\*假設你每天睡 6 小時，而且每天都使用 CatDesk。那麼你每小時可以傳送 3,000 / (24 - 6) / 7 = 23.8 則訊息。由於 Thinking 和工具呼叫本身需要時間，因此要真的把每週 3,000 則訊息用完其實非常困難。

# 類似專案

如果你不想使用 CatDesk，也可以試試以下類似專案：

| 專案 | 說明 |
| --- | --- |
| [Desktop Commander](https://github.com/wonderwhy-er/DesktopCommanderMCP) | 通用型 MCP server，可操作本機檔案系統、終端機、程序管理、編輯與自動化。 |
| [DevSpace](https://github.com/Waishnav/devspace) | 自架 MCP server，讓 ChatGPT 和其他支援 MCP 的宿主獲得類似 Codex 的開發流程。 |
| [CodexPro](https://github.com/rebel0789/codexpro) | 給 ChatGPT 使用的本機 MCP 程式開發工具，只能操作明確允許的 repository。 |
| [ChatGPT Local Coder](https://github.com/hoangcoderr/chatgpt-local-coder) | 自架 MCP server，讓 ChatGPT Web 使用檔案系統、Shell、Git、patch 與專案上下文工具。 |
| [Local Coding Agent](https://github.com/LongNgn204/local-coding-agent) | 給 ChatGPT Web 和其他 MCP client 使用的本機開發 workspace。 |
| [Proxide](https://github.com/tt-a1i/proxide) | 與 agent 無關的 workspace bridge，可讓網頁版模型透過 MCP 或 browser fallback 操作本機 repository。 |
| [codex-mcp](https://github.com/mollehxh/codex-mcp) | 小型 MCP server，透過 stdio 或 HTTP 提供類 Codex workspace 介面。 |

> [!NOTE]
> 上述專案皆不屬於我，我也不是其維護者。列在這裡僅供參考。

# 誰會需要這個？

- Codex 額度重置沒幾天就用完的人（我🥺）
- 做 Web 開發或爬蟲的人。（CatDesk 透過 chrome-devtools-mcp 整合，讓 ChatGPT Web 可以讀取網頁元素並控制瀏覽器分頁。）

# 快速開始

> [!CAUTION]
> 這個工具權限非常高，理論上可以把你的整顆硬碟刪光，或產生其他非預期結果。
> 請在 VM 或容器內執行（DevContainer 是個不錯的選擇）。
> 把它當成 OpenClaw 一樣對待，保持容器化與隔離。

1. 用 npm 全域安裝 CatDesk。

   ```bash
   npm install -g catdesk
   ```

2. 在任意終端機目錄執行 CatDesk。

   ```bash
   catdesk
   ```

   CatDesk 啟動後，可以選擇 `Control Computer`、`Control Browser` 或 `Both`。在模式選擇畫面按 `l` 可以在 English 與繁體中文之間切換；語言偏好會儲存在 `~/.catdesk/config.toml`。如果啟用了瀏覽器控制，請選擇一個支援的 Chromium 瀏覽器。在 macOS 上，除了 `PATH` 中的 binary，CatDesk 也會偵測 `/Applications` 與 `~/Applications` 裡的標準瀏覽器 App bundle。

   第一次啟動時，CatDesk 會要求你輸入 **ngrok authtoken** 和 **ngrok static domain**（例如 `my-app.ngrok-free.dev`）。這兩項都可以從 [ngrok dashboard](https://dashboard.ngrok.com/get-started/setup) 取得，並會儲存在 `~/.catdesk/config.toml`，之後啟動時自動重用。

   CatDesk 預設監聽 `3200` port。可以用 `PORT` 覆寫。Workspace root 預設為你啟動 CatDesk 時所在的目錄，也可以用 `WORKSPACE_ROOT` 覆寫。

   第一次從 macOS Terminal.app 啟動時，CatDesk 會詢問你是否要使用專用的 `CatDesk` Terminal profile，並把選擇儲存在 `~/.catdesk/config.toml`。如果啟用，而且目前分頁尚未使用該 profile，CatDesk 會套用它、關閉暫時建立的 helper window，並要求你在該分頁再次執行相同指令。之後啟動時會直接重用已儲存的偏好。設定 `CATDESK_SKIP_MACOS_TERMINAL_PROFILE=1` 可以暫時保留目前的 Terminal session，不論已儲存的偏好為何。

3. 等待 TUI 顯示 MCP Server URL。

4. 開啟 [ChatGPT connector settings](https://chatgpt.com/plugins#settings/Connectors?create-connector=true&redirectAfter=%2Fplugins)。

5. 在彈出視窗中填寫 connector 表單：
   - Name：`CatDesk`，或任何你喜歡的名稱
   - MCP Server URL：CatDesk TUI 顯示的完整 URL
   - Authentication：`None`

6. 點擊 `I understand and want to continue`。

7. 點擊 `Create`，再點擊 `Connect`。

   - 權限預設為 **Allow read actions**。如果想要最順暢的體驗，我建議使用 **Allow all actions**（等同 Codex 的 `--yolo`；請小心使用）。

8. 把以下內容加入 ChatGPT 的 `Custom instructions`：

```text
CatDesk is a coding tool and a custom connector. Always use CatDesk if the user wants to do anything related to file operations. Always call `catdesk_instruction` after `list_resources`, and follow the instructions it contains.
```

9. 開始在 ChatGPT Web 中使用 connector。幾個重要提示：

- 我建議讓 ChatGPT 自動決定要使用哪個 connector。你也可以用 `/` 或 `@` 手動選 CatDesk。這樣 ChatGPT 只能存取你手動指定的 connector，穩定性可能更好。缺點是 `web.search` 和 `web.open` 會被停用，也就是無法搜尋最新資訊。`web` 工具與 custom connector 不能同時使用。

<table align="center">
  <tr>
    <td align="center">
      <img src="docs/images/connector_slash.png" alt="Select CatDesk from the slash command menu" width="300"><br>
      <em>使用 <code>/</code> 手動選擇 CatDesk</em>
    </td>
    <td align="center">
      <img src="docs/images/connector_at.png" alt="Select CatDesk from the at-sign menu" width="300"><br>
      <em>使用 <code>@</code> 手動選擇 CatDesk</em>
    </td>
  </tr>
</table>

- 為了提升效能並避免記憶體使用量過高，我強烈建議**每個小功能都開一個新 session**。切換聊天前，可以請 ChatGPT 使用 `create_handoff`。CatDesk 會把目前目標、已完成工作、重要決策、驗證結果、下一步以及 Git 狀態整理到目前 workspace 裡的 `.catdesk/handoff.md`。下一個 session 呼叫 `catdesk_instruction` 時會偵測這份 handoff，並提醒 ChatGPT 先讀取後再繼續。不要把憑證、token、密碼或其他秘密寫進 handoff。CatDesk 本身不會修改 `.gitignore`；`catdesk_instruction` 會要求 ChatGPT 在建立 handoff 前確認 `.catdesk/` 已被忽略，除非你明確希望追蹤它。工具呼叫超過 50 次之後，畫面可能會開始非常卡。
<p align="center">
  <img src="docs/images/high_ram_usage.png" alt="3.9 GB Memory usage🥹" width="300"><br>
  <em>3.9 GB 記憶體用量🥹</em>
</p>

- 如果你修改了 MCP 相關設定（包含 tool mode 或啟用／停用 widget），需要開一個新聊天，並在 [settings](https://chatgpt.com/#settings/Plugins) 中刷新 CatDesk。最可靠的方式是刪除 CatDesk 後重新安裝（步驟 2–7）。

<table align="center">
  <tr>
    <td align="center">
      <img src="docs/images/refresh_catdesk.png" alt="Refresh CatDesk in ChatGPT settings" width="500"><br>
      <em>在 ChatGPT 設定中刷新 CatDesk</em>
    </td>
    <td align="center">
      <img src="docs/images/remove_catdesk.png" alt="Remove CatDesk from ChatGPT settings" width="500"><br>
      <em>從 ChatGPT 設定中移除 CatDesk</em>
    </td>
  </tr>
</table>

# 技術棧

| 部分 | 技術 |
| --- | --- |
| Core | Rust |
| MCP server | 自訂實作（不使用 SDK） |
| MCP protocolVersion | `2026-07-28` |
| Server | Axum + Tokio |
| TUI | Ratatui |
| Tunnel | ngrok |
| 瀏覽器控制 | chrome-devtools-mcp |
| Widget | HTML + JavaScript |
| 發布方式 | npm |

# 工具

CatDesk 有兩種本機工具模式：`multi-tools` 提供 11 個工具，`read-only` 提供 3 個工具。

在 `multi-tools` 模式下，CatDesk 的本機工具如下：

| 工具                    | 類型  | 功能                                                                     |
| ----------------------- | ----- | ------------------------------------------------------------------------ |
| `catdesk_instruction`   | 指南  | 回傳 CatDesk 使用說明並顯示 Binagotchy                                  |
| `read`                  | 讀取  | 從 workspace 讀取一個或多個文字檔                                       |
| `search`                | 讀取  | 使用 `rg`、`grep` 或內建搜尋器搜尋 workspace 文字                        |
| `write`                 | 寫入  | 建立或覆寫檔案                                                           |
| `edit`                  | 寫入  | 原子化套用受保護的 replace/range 編輯                                   |
| `create_handoff`        | 寫入  | 儲存結構化 session 上下文與目前 Git 狀態，供下一個聊天接續               |
| `delete`                | 寫入  | 刪除檔案或目錄                                                           |
| `run_command`           | Shell | 執行短時間 Shell 指令並等待完成                                          |
| `start_command`         | Job   | 啟動長時間指令，並立即回傳 job ID                                        |
| `poll_command`          | Job   | 讀取背景指令的增量輸出與狀態                                             |
| `cancel_command`        | Job   | 停止背景指令以及其子程序樹                                               |

長時間執行的指令刻意與 MCP HTTP request 的生命週期分離。Build、compile、dependency installation、長時間 test suite 與 development server 應使用 `start_command`，接著以回傳的 cursor 呼叫 `poll_command`。Poll response 有大小限制；如果 `hasMoreOutput` 為 true，即使 job 已經結束，也要持續用 `nextCursor` 輪詢，直到把剩餘輸出讀完。`run_command` 適合較短的指令，而且 timeout 上限為 120 秒。

如果啟用了瀏覽器模式，CatDesk 還可以公開額外的 browser/devtools 工具。這些工具由 browser bridge 提供，所以實際工具列表取決於你的環境。

`search` 會優先使用 `rg`，如果沒有則退回 `grep`，最後才使用 CatDesk 內建搜尋器。安裝 ripgrep 並非必要，但能提供最佳的搜尋效能與行為。

# Context window

根據[這篇文章](<https://help.openai.com/en/articles/11909943-gpt-53-and-gpt-54-in-chatgpt#:~:text=Thinking%20(GPT%E2%80%915.4%20Thinking)>)以及[這段程式碼](https://github.com/openai/codex/blob/main/codex-rs/models-manager/src/model_info.rs#L85)，ChatGPT Web 與 Codex 的 context window 並不相同。

| 方案 | CatDesk + ChatGPT Web（in + out = 總和） | Codex CLI（總和）      |
| ---- | ---------------------------------------- | ---------------------- |
| Plus | 128K + 128K = 256K                       | 258K（1M experimental） |
| Pro  | 272K + 128K = 400K                       | 258K（1M experimental） |

# FAQ

## 可以關掉紅色 CSP 按鈕嗎？

<table align="center">
  <tr>
    <td align="center">
      <img src="docs/images/csp_button.png" alt="The red CSP button shown in tool calls" height="96"><br>
      <em>紅色 CSP 按鈕</em>
    </td>
    <td align="center">
      <img src="docs/images/enforce_csp.png" alt="Advanced connector settings with Enforce CSP in developer mode" height="96"><br>
      <em>Advanced connector settings 中的 <code>Enforce CSP in developer mode</code></em>
    </td>
  </tr>
</table>

可以。開啟 [Advanced connector settings](https://chatgpt.com/#settings/Connectors/Advanced)，然後啟用 `Enforce CSP in developer mode`。這個設定會移除紅色按鈕。CatDesk 會自動把目前的 ngrok domain 加進 widget CSP，因此開啟 CSP enforcement 後 widget 應該仍能正常運作。

## 我明明已經連過了，為什麼還一直叫我重新 Connect？

目前看不出 connector 什麼時候會觸發 `Connect` 有明顯規律。我可以確定它不是依照 tool call 次數觸發，但具體原因我也不知道。

<table align="center">
  <tr>
    <td align="center">
      <img src="docs/images/connect1.png" alt="Connector asks to connect again" width="700"><br>
      <em>Connector 再次要求連線</em>
    </td>
    <td align="center">
      <img src="docs/images/connect2.png" alt="Connector asks to connect again (After you click Continue)" width="700"><br>
      <em>Connector 再次要求連線（點擊 Continue 之後）</em>
    </td>
  </tr>
</table>

看起來這是一個 bug，而且他們已經修好了 🥳。

## CatDesk 可以用在其他 App 嗎？

理論上可以。CatDesk 也可能能和其他支援自訂遠端 MCP server 的 App 搭配，包括 Claude。（雖然我不覺得有人會拿 CatDesk 配 Claude，因為 Claude Chat 模式和 Claude Code 共用同一份使用額度。）

不過 CatDesk 是專門為 ChatGPT Chat 以及它的 Custom Connector 流程打造的。（它們先把這個功能改名為 _Apps_，後來又改名成 _Plugins_，但為了避免和 _Application_ 混淆，我還是比較喜歡叫它 _Connector_。）ChatGPT Chat 是 CatDesk 主要設計與測試的環境，因此其他 App 的體驗可能沒有這麼順。

## Input/output token 是怎麼算的？

CatDesk 無法從 ChatGPT Web 取得官方 token usage 數字。它會在本機使用 `o200k_base` 估算，這和 GPT-5.5 類模型使用的是同一 tokenizer family，因此數字有參考價值，但仍然只是估算值。

| 欄位           | 符號 | 意義                         | 價格                           |
| -------------- | ---- | ---------------------------- | ------------------------------ |
| `inputTokens`  | `↓`  | Tool input ≈ LLM output      | ≈ `$30.00 / 1M` output tokens  |
| `outputTokens` | `↑`  | Tool output ≈ LLM input      | ≈ `$5.00 / 1M` input tokens    |
| `totalTokens`  | `Σ`  | `inputTokens + outputTokens` | `input price + output price`   |

CatDesk 不會計算：

- 完整的 ChatGPT 對話
- 隱藏 prompt 或 reasoning token
- OpenAI 端其他內部 token

載入動畫只是視覺效果。ChatGPT Web 不會把部分 MCP tool input/output 即時串流給 CatDesk，所以 widget 會先在本機播放動畫，等真正的 tool result 回來後再鎖定到估算值。

## Workspace 是什麼？

Workspace 是 CatDesk 被允許操作的根目錄。

預設就是你啟動 CatDesk 時所在的目錄。也可以用 `WORKSPACE_ROOT` 覆寫。

File tool 都會以此目錄為 base path，超出 workspace 的路徑會被拒絕。

## AGENTS.md 要放哪裡？

可以放在 3 個位置：

1. Workspace root
2. `~/.catdesk/AGENTS.md`
3. `~/.codex/AGENTS.md`

CatDesk 會按照上述順序尋找 `AGENTS.md`，每次呼叫 `catdesk_instruction` 都會重新檢查。你也可以手動選擇要使用哪一份 `AGENTS.md`。

<p align="center">
  <img src="docs/images/set_agents_md.png" alt="Set AGENTS.md manually" width="500"><br>
  <em>手動設定 AGENTS.md</em>
</p>

## Widget 一片空白怎麼辦？

<p align="center">
  <img src="docs/images/blank_widget.png" alt="Empty widget/function call" width="500"><br>
  <em>空白 widget/function call</em>
</p>

1. 直接重新整理頁面並重新連接 connector。
2. 停止回覆，再重新傳送訊息。

這是 ChatGPT 端的 bug，我這邊沒有辦法修，改 CatDesk 程式碼也解決不了。這個 bug 可能是在 4 月 15 日左右出現的。

# 安全性

> [!CAUTION]
> **絕對不要**把 `MCP Server URL` 分享給任何人。任何拿到這個 URL 的人都可能存取你的電腦。

URL 由以下部分組成：

| 部分         | 範例                          | 意義                           |
| ------------ | ----------------------------- | ------------------------------ |
| Public URL   | `https://xxxx.ngrok-free.dev` | 你的 ngrok static domain       |
| Random path  | `/Ab3kL9xQ2pTm7VhC`           | 第一次啟動時隨機產生的路徑     |
| MCP endpoint | `/mcp`                        | 真正的 MCP endpoint            |

因此完整 URL 會長這樣：

```text
https://xxxx.ngrok-free.dev/Ab3kL9xQ2pTm7VhC/mcp
```

Static domain 和 random path 都會儲存在 `~/.catdesk/config.toml`，所以完整 MCP URL 在每次啟動後都會保持不變。Connector 只需要設定一次。

# 關於 Binagotchy

<p align="center">
  <img src="docs/images/binagotchy.gif" alt="Binagotchy!" width="500"><br>
  <em>Binagotchy!</em>
</p>

這個角色是一隻可愛的鯊魚貓！其實我在做 CatDesk 之前就已經做了它，後來決定把它放進這個專案。

CatDesk 預設會在每次啟動時產生一隻隨機 Binagotchy。如果你看到喜歡的，可以在啟動畫面把它設成你的夥伴。系統也會把每一隻 Binagotchy 自動儲存到 `~/.catdesk/binagotchy`。你也可以下載它（更精確地說，是匯出）！支援 `.png` 和 `.gif`。歡迎拿去任何地方使用。這個專案與 Binagotchy 都採用 MIT License。順帶一提，Binagotchy 完全由腳本生成，沒有使用任何 text-to-image 或 diffusion model。
