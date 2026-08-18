# CatDesk

An open-source tool that lets you use ChatGPT Chat as a local coding agent. No reverse engineering, no API, no Codex, no Work mode. A ChatGPT Plus subscription is enough.

<p align="center">
  <img src="docs/images/catdesk_preview.gif" alt="CatDesk in ChatGPT Web"><br>
  <em>CatDesk in ChatGPT Web</em>
</p>

# Disclaimer

This is an independent open-source project and is not affiliated with or endorsed by OpenAI. I built it as a personal tool and decided to open-source it. Some features are still buggy and may cause unexpected behavior. Use it at your own risk. I am not responsible for any loss caused by this tool. It is strongly recommended to run it inside a VM or container.

# Why CatDesk?

Codex has a very generous weekly quota (reset usage frequently) compared to Antigravity (good at good morning) and Claude Code (RIP 5h quota 💀), that's why I love OpenAI so much.

<p align="center">
  <img src="docs/images/codex_2x_usage.png" alt="Codex reset usage frequently🙏" width="700"><br>
  <em>Codex reset usage frequently🙏</em>
</p>

However, the quota runs out very quickly if you work on a large project.

<p align="center">
  <img src="docs/images/no_remaining_usage.png" alt="I used up my Codex quota on the first day after it reset" width="700"><br>
  <em>I used up my Codex quota on the first day after it reset</em>
</p>

Then you need to wait another 7 days. What are you going to do for the rest of the week?

Here's the solution: most people with a Plus subscription do not use even 10% of their weekly thinking messages.

**_So why not use your 3,000 weekly messages for coding?_**

That's the idea behind CatDesk! It gives ChatGPT Web tools like `write` and `run_command` to edit files on your computer.

<p align="center">
  <img src="docs/images/thinking_usage_limits.png" alt="ChatGPT reasoning usage limits for GPT-5.5 and GPT-5.6" width="900"><br>
  <em>GPT-5.5: <a href="https://web.archive.org/web/20260519111010/https://help.openai.com/en/articles/11909943-gpt-55-in-chatgpt">3,000 messages/week</a>, GPT-5.6: <a href="https://help.openai.com/en/articles/20001354-gpt-56-in-chatgpt">unknown</a> but I have never hit the limit</em>
</p>

# How does this work?

1. A ChatGPT Plus or above subscription is required.
2. CatDesk runs as a local MCP server on your computer. It has the ability to run commands and edit files, just like Codex.
3. You can connect ChatGPT Web to CatDesk using a Custom Connector, which is a feature available only to Plus and Pro users.
4. Done! Now ChatGPT Web can control your computer and code on it.

In short,

```text
ChatGPT Web + CatDesk
= a stripped-down version of Codex
= OpenClaw without cron and other active utilities
```

I tried this with GPT-5.2 before, and the results were poor. However, **GPT-5.4 Thinking is now really good at tool calling and computer use.** The first time I tried it with GPT-5.4, I was honestly surprised by how well it worked. GPT-5.5 and GPT-5.6 are even smoother, and GPT-5.6 is extremely good at using CatDesk. It's also very fast.

# Differences between ChatGPT Chat + CatDesk, Codex, and the API (let's say Plus plan)

|       | ChatGPT Chat + CatDesk                             | Codex                   | OpenAI API           |
| ----- | -------------------------------------------------- | ----------------------- | -------------------- |
| Usage | 3,000 messages/week                                | Generous weekly quota   | Pay as you go        |
| Pros  | Stable, no extra fee, and nearly unlimited\* quota | Stable and no extra fee | Stable               |
| Cons  | Not as smooth as native Codex                      | Runs out very quickly   | Tokens are expensive |

\*Let's say you sleep 6 hours a day and use CatDesk every day. In that case, you can send 3,000 / (24 - 6) / 7 = 23.8 messages per hour. Since thinking and tool calls take time, it is very difficult to use up your weekly 3,000 message limit.

# Who needs this?

- People who used up their Codex quota on the first few day after it reset (me🥺)
- People who are working on web development and crawlers. (CatDesk enables ChatGPT Web to read elements and control your browser tab through chrome-devtools-mcp integration.)

# Quickstart

> [!CAUTION]
> This tool is very powerful and can potentially wipe your whole disk or produce unexpected results.
> Run it inside a VM or container (DevContainer is a good option).
> Treat it like OpenClaw, keep it containerized and isolated.

1. Install CatDesk globally with npm.

   ```bash
   npm install -g catdesk
   ```

2. Run CatDesk from any terminal directory.

   ```bash
   catdesk
   ```

   When CatDesk starts, choose `Control Computer`, `Control Browser`, or `Both`. If browser control is enabled, select a supported Chromium browser.

   On first launch, CatDesk will ask you to enter your **ngrok authtoken** and **ngrok static domain** (e.g. `my-app.ngrok-free.dev`). You can get both from the [ngrok dashboard](https://dashboard.ngrok.com/get-started/setup). These are saved to `~/.catdesk/config.toml` and reused on subsequent launches.

   By default, CatDesk listens on port `3200`. You can override it with `PORT`. The workspace root defaults to the current working directory and can be overridden with `WORKSPACE_ROOT`.

   On macOS Terminal.app, CatDesk manages a dedicated `CatDesk` Terminal profile automatically. If the current Terminal tab is not already using that profile, CatDesk applies it, closes any temporary helper window, and asks you to run the same command again in that tab. It only starts immediately when the current tab is already using `CatDesk`. Set `CATDESK_SKIP_MACOS_TERMINAL_PROFILE=1` if you want to keep the current Terminal session untouched.

3. Wait for the TUI to show the MCP Server URL.

4. Open [ChatGPT connector settings](https://chatgpt.com/plugins#settings/Connectors?create-connector=true&redirectAfter=%2Fplugins).

5. In the pop-up window, fill in the connector form:
   - Name: `CatDesk` or any name you like
   - MCP Server URL: the full URL shown in CatDesk TUI
   - Authentication: `None`

6. Click `I understand and want to continue`.

7. Click `Create`, then click `Connect`.

   - Permission defaults to **Allow read actions**. For the smoothest experience, I recommend **Allow all actions** (equivalent to Codex's `--yolo`; use with caution).

8. Add this to your ChatGPT `Custom instructions`:

```text
CatDesk is a coding tool and a custom connector. Always use CatDesk if the user wants to do anything related to file operations. Always call `catdesk_instruction` after `list_resources`, and follow the instructions it contains.
```

9. Start using the connector from ChatGPT Web. Some important tips:

- I recommend let ChatGPT to decide which connector automatically. You can manually selecting the connector using `/` or `@`. This way, ChatGPT can only access the connector you selected, which may improve stability. However, the downside is, `web.search` and `web.open` will be disabled. Which means it can't search latest info. The `web` tool and a custom connector cannot be used at the same time.

<table align="center">
  <tr>
    <td align="center">
      <img src="docs/images/connector_slash.png" alt="Select CatDesk from the slash command menu" width="300"><br>
      <em>Select CatDesk manually with <code>/</code></em>
    </td>
    <td align="center">
      <img src="docs/images/connector_at.png" alt="Select CatDesk from the at-sign menu" width="300"><br>
      <em>Select CatDesk manually with <code>@</code></em>
    </td>
  </tr>
</table>

- To improve performance and avoid high memory usage, I strongly recommend **opening a new session for every small feature**. If you need context, you can ask ChatGPT to create a handoff note and paste it into the new session. It will become extremely laggy after 50+ tool calls.
<p align="center">
  <img src="docs/images/high_ram_usage.png" alt="3.9 GB Memory usage🥹" width="300"><br>
  <em>3.9 GB Memory usage🥹</em>
</p>


# Stack

| Part | Stack |
| --- | --- |
| Core | Rust |
| MCP server | Custom implementation (no SDK) |
| MCP protocolVersion | `2025-11-25` |
| Server | Axum + Tokio |
| TUI | Ratatui |
| Tunnel | ngrok |
| Browser control | chrome-devtools-mcp |
| Widget | HTML + JavaScript |
| Distribution | npm |

# Tools

CatDesk has two local tool modes: `multi-tools` exposes 10 tools, and `read-only` exposes 3 tools.

CatDesk's local tools in `multi-tools` mode are:

| Tool                  | Type  | What it does                                                               |
| --------------------- | ----- | -------------------------------------------------------------------------- |
| `catdesk_instruction` | Guide | Returns CatDesk usage instructions and render Binagotchy                   |
| `read`                | Read  | Reads a text file from the workspace                                       |
| `search`              | Read  | Searches workspace text with `rg`, `grep`, or built-in search              |
| `write`               | Write | Creates or overwrites a file                                               |
| `edit`                | Write | Replaces exact text inside a file                                          |
| `delete`              | Write | Deletes a file or directory                                                |
| `run_command`         | Shell | Runs a short shell command and waits for completion                        |
| `start_command`       | Job   | Starts a long-running shell command and immediately returns a job ID       |
| `poll_command`        | Job   | Reads incremental output and status from a background command              |
| `cancel_command`      | Job   | Stops a background command and its child process tree                      |

Long-running commands are deliberately decoupled from the lifetime of an MCP HTTP request. Builds, compilation, dependency installation, long test suites, and development servers should use `start_command`, then `poll_command` with the returned cursor. Poll responses are bounded; if `hasMoreOutput` is true, keep polling with `nextCursor` even after the command reaches a terminal state to drain the remaining buffered output. `run_command` remains the simpler path for short commands and has a 120-second maximum timeout.

If browser mode is enabled, CatDesk can also expose extra browser/devtools tools. Those are provided by the browser bridge, so the exact list depends on your environment.

`search` uses `rg` when it is available, falls back to `grep`, then falls back to CatDesk's built-in scanner. Installing ripgrep is optional, but gives the best search performance and behavior.

# Context window

According to [the blog](<https://help.openai.com/en/articles/11909943-gpt-53-and-gpt-54-in-chatgpt#:~:text=Thinking%20(GPT%E2%80%915.4%20Thinking)>) and [the code](https://github.com/openai/codex/blob/main/codex-rs/models-manager/src/model_info.rs#L85), the context window in ChatGPT web is different from Codex.

| Tier | CatDesk + ChatGPT Web (in + out = sum) | Codex CLI (sum)        |
| ---- | -------------------------------------- | ---------------------- |
| Plus | 128K + 128K = 256K                     | 258K (1M experimental) |
| Pro  | 272K + 128K = 400K                     | 258K (1M experimental) |

# FAQ

### Can I turn off the red CSP button?

<table align="center">
  <tr>
    <td align="center">
      <img src="docs/images/csp_button.png" alt="The red CSP button shown in tool calls" height="96"><br>
      <em>The red CSP button</em>
    </td>
    <td align="center">
      <img src="docs/images/enforce_csp.png" alt="Advanced connector settings with Enforce CSP in developer mode" height="96"><br>
      <em><code>Enforce CSP in developer mode</code> in Advanced connector settings</em>
    </td>
  </tr>
</table>

Yes. Open [Advanced connector settings](https://chatgpt.com/#settings/Connectors/Advanced) and turn on `Enforce CSP in developer mode`. That setting removes the red button. CatDesk automatically adds the current ngrok domain to the widget CSP, so the widget should keep working with CSP enforcement enabled.

### I've already connected. Why do I need to connect again and again?

There doesn't seem to be any obvious pattern for when the connector triggers `Connect`. I'm sure it's not triggered by the tool call count, but I don't know the exact reason.

<table align="center">
  <tr>
    <td align="center">
      <img src="docs/images/connect1.png" alt="Connector asks to connect again" width="700"><br>
      <em>Connector asks to connect again</em>
    </td>
    <td align="center">
      <img src="docs/images/connect2.png" alt="Connector asks to connect again (After you click Continue)" width="700"><br>
      <em>Connector asks to connect again (After you click Continue)</em>
    </td>
  </tr>
</table>

I know it’s annoying. I’m trying to find a solution now.

### Can CatDesk be used in other apps?

Yes, in theory. CatDesk may also work with other apps that support custom remote MCP servers, including Claude. (I don't think anyone will use CatDesk with Claude though, since Claude Chat mode and Claude Code share the same usage limits.)

However, CatDesk is built specifically for ChatGPT Chat and its Custom Connector (They renamed it to _Apps_, and now they renamed it again and call it _Plugins_, but to prevent confusion with _Application_, I still prefer call it _Connector_) flow. ChatGPT Chat is the environment CatDesk is designed and tested for, so other apps may not work as smoothly.

### How does the input/output token be calculated?

CatDesk does not get official token usage numbers from ChatGPT Web. It estimates them locally with `o200k_base`, the same tokenizer family used by GPT-5.5-style models, so the numbers are useful, but still only estimates.

| Field          | Symbol | What it means                | Price                         |
| -------------- | ------ | ---------------------------- | ----------------------------- |
| `inputTokens`  | `↓`    | Tool input ≈ LLM output      | ≈ `$30.00 / 1M` output tokens |
| `outputTokens` | `↑`    | Tool output ≈ LLM input      | ≈ `$5.00 / 1M` input tokens   |
| `totalTokens`  | `Σ`    | `inputTokens + outputTokens` | `input price + output price`  |

CatDesk does not count:

- the full ChatGPT conversation
- hidden prompts or reasoning tokens
- other internal tokens on OpenAI's side

The loading animation is only a visual effect. ChatGPT Web does not stream partial MCP tool input/output into CatDesk, so the widget animates locally first and then locks to the estimated values when the real tool result arrives.

### What is workspace?

Workspace is the root directory CatDesk is allowed to work in.

By default, it is the directory where you launch CatDesk. You can also override it with `WORKSPACE_ROOT`.

File tools use this directory as their base path, and paths outside the workspace are rejected.

### Where to put my AGENTS.md?

You can put it in 3 places.

1. Workspace root
2. `~/.catdesk/AGENTS.md`
3. `~/.codex/AGENTS.md`

CatDesk checks these locations for `AGENTS.md` in this order. This happens every time `catdesk_instruction` is called. You can also manually choose which `AGENTS.md` to use.

<p align="center">
  <img src="docs/images/set_agents_md.png" alt="Set AGENTS.md manually" width="500"><br>
  <em>Set AGENTS.md manually</em>
</p>

### What to do if the widget is blank?

<p align="center">
  <img src="docs/images/blank_widget.png" alt="Empty widget/function call" width="500"><br>
  <em>Empty widget/function call</em>
</p>

1. Simply refresh the page and reconnect the connector.
2. Stop the response and send the message again.

This is a bug on ChatGPT's side. There is nothing I can do about it, and changing the code will not solve the issue. This bug was probably introduced on Apr 15th.

# Safety

> [!CAUTION]
> Do **NOT** share the `MCP Server URL` with anyone. Anyone with the URL can access your computer.

The URL is made of these parts:

| Part         | Example                       | What it means                                |
| ------------ | ----------------------------- | -------------------------------------------- |
| Public URL   | `https://xxxx.ngrok-free.dev` | Your ngrok static domain                     |
| Random path  | `/Ab3kL9xQ2pTm7VhC`           | A random path generated on first launch      |
| MCP endpoint | `/mcp`                        | The actual MCP endpoint                      |

So the full URL looks like this:

```text
https://xxxx.ngrok-free.dev/Ab3kL9xQ2pTm7VhC/mcp
```

Both the static domain and the random path are persisted in `~/.catdesk/config.toml`, so the full MCP URL stays the same across launches. You only need to set up the connector once.

# About Binagotchy

<p align="center">
  <img src="docs/images/binagotchy.gif" alt="Binagotchy!" width="500"><br>
  <em>Binagotchy!</em>
</p>

The character is a cute shark-cat! I actually made this before CatDesk and decided to put it in the project.

By default, CatDesk will generate a random Binagotchy every time you start it. If you see a cute one, you can set it as your partner on the launch screen. The system will also automatically save every Binagotchy in `~/.catdesk/binagotchy`. You can download it too (or, to be accurate, export it)! Both `.png` and `.gif` are supported. Feel free to use it anywhere. This project and Binagotchy are both under the MIT License. By the way, Binagotchy is generated using pure scripts and does not use any text-to-image or diffusion model.
