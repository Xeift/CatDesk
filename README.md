# CatDesk

An open-source tool that turns ChatGPT Web into a coding agent. No API, no Codex. A ChatGPT Plus subscription is enough. Just use your 3,000 weekly Thinking messages in ChatGPT Web.

# Disclaimer

This is an independent open-source project and is not affiliated with or endorsed by OpenAI. I built it as a personal tool and decided to open-source it. Some features are still buggy and may cause unexpected behavior. Use it at your own risk. I am not responsible for any loss caused by this tool. It is strongly recommended to run it inside a VM or container.

# Who needs this?

- People who used up their Codex quota on the first day after it reset (me🥺)
- People who are working on web development and crawlers. (CatDesk enables ChatGPT Web to read elements and control your browser tab through chrome-devtools-mcp integration.)

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

```text
Your computer --ngrok--> ChatGPT Web
catdesk
```

I tried this with GPT-5.2 before, and the results were poor. However, **GPT-5.4 Thinking is now really good at tool calling and computer use.** The first time I tried it with GPT-5.4, I was honestly surprised by how well it worked.

# Differences between ChatGPT + CatDesk, Codex, and the API (let's say Plus plan)

Codex has a very generous quota compared to Antigravity. However, the quota runs out very quickly if you work on a large project. Most people with a Plus subscription do not use even 10% of their weekly thinking messages.

So why not use your 3,000 weekly messages for coding?

|       | ChatGPT + CatDesk                                | Codex                   | OpenAI API           |
| ----- | ------------------------------------------------ | ----------------------- | -------------------- |
| Usage | 3,000 messages/week                              | Generous weekly quota   | Pay as you go        |
| Pros  | Stable, no extra fee, and nearly unlimited quota | Stable and no extra fee | Stable               |
| Cons  | Not as smooth as native Codex                    | Runs out very quickly   | Tokens are expensive |

# Quickstart

> [!CAUTION]
> This tool is very powerful and can potentially wipe your whole disk or produce unexpected results.
> Run it inside a VM or container (DevContainer is a good option).
> Treat it like OpenClaw, keep it containerized and isolated.

1. Install prerequisites.
   You need `ngrok` available in your `PATH`. CatDesk launches ngrok itself from the local machine, see [`ngrok::start()`](/home/xeift/Desktop/MCP3000/src/ngrok.rs#L5).

2. Download the CatDesk binary for your platform.
   Put it anywhere you want. If your platform requires it, make the binary executable first.

3. Run the CatDesk binary.

   ```bash
   ./catdesk
   ```

   By default, CatDesk listens on port `3200`, as defined in [`main()`](/home/xeift/Desktop/MCP3000/src/main.rs#L325). You can override it with `PORT`. The workspace root defaults to the current working directory and can be overridden with `WORKSPACE_ROOT`, also in [`main()`](/home/xeift/Desktop/MCP3000/src/main.rs#L325).

4. Wait for the TUI to show the MCP Server URL.
   CatDesk starts the local server and ngrok automatically in [`start_services()`](/home/xeift/Desktop/MCP3000/src/main.rs#L1425). The MCP URL is shown in the UI at [`draw_ui()`](/home/xeift/Desktop/MCP3000/src/main.rs#L1748), and the actual value includes a random runtime slug, generated in [`generate_mcp_slug()`](/home/xeift/Desktop/MCP3000/src/state.rs#L501). Because ngrok URLs and the slug both change between runs, you should treat the URL as ephemeral.

5. Open ChatGPT connector settings.

   `https://chatgpt.com/apps#settings/Connectors`

   This matches the built-in guide rendered in [`guide_lines`](/home/xeift/Desktop/MCP3000/src/main.rs#L2150).

6. Click `Create app`.

7. Fill in the connector form.
   Use:
   - Name: `CatDesk` or any name you like
   - MCP Server URL: the full URL shown by CatDesk
   - Authentication: `None`

   These values come directly from the in-app guide in [`guide_lines`](/home/xeift/Desktop/MCP3000/src/main.rs#L2150).

8. Click `I understand and want to continue`.

9. Click `Create`.

10. Add this to your connector system instruction:

```text
Always call catdesk_instruction after list_resources, and follow the instructions it contains.
```

11. Start using the connector from ChatGPT Web.

# Built-in tools

CatDesk's built-in local tools are:

| Tool | Type | What it does |
| ----- | ----- | ----- |
| `catdesk_instruction` | Guide | Returns CatDesk usage instructions and reads your workspace `AGENTS.md` |
| `read_file` | Read | Reads a text file from the workspace |
| `list_files` | Read | Recursively lists files and directories in the workspace |
| `search_text` | Read | Searches text across files in the workspace |
| `write_file` | Write | Creates or overwrites a file |
| `append_file` | Write | Appends text to a file, or creates it if missing |
| `make_directory` | Write | Creates a directory |
| `move_path` | Write | Moves or renames a file or directory |
| `delete_path` | Write | Deletes a file or directory |
| `replace_in_file` | Write | Replaces text inside a file |
| `run_command` | Shell | Runs a shell command inside the workspace. Use it as a last resort when a dedicated tool cannot do the job |

If browser mode is enabled, CatDesk can also expose extra browser/devtools tools. Those are provided by the browser bridge, so the exact list depends on your environment.

# FAQ

### Can I turn off the red CSP button?

No. That button is not part of the widget, so CatDesk cannot control it. I agree it looks annoying, but there is nothing this project can do about it right now.

### Can I skip approval? Like `--yolo` or `--dangerously-skip-permissions`?

No. This restriction comes from the ChatGPT Web side. There is not much CatDesk can do about it. They probably use an LLM or some internal policy layer to detect higher-risk operations and require manual approval. Sometimes it is annoying, but there is no good workaround right now.

### Can CatDesk be used in other apps?

No. CatDesk is built around ChatGPT Web and its Custom Connector (They call it _Apps_ now, but to prevent confusion with _Application_, I still prefer call it _Connector_) flow. In practice, that means this project is not just a plain standalone MCP server. Also, there still are not many AI apps that support custom remote MCP servers well. Even if they support, they probably does not provide such generous (3000 messages) weekly quota.

For Claude, web and Claude Code share same quota, so just simply use Claude Code, no need to use CatDesk.

### How does the input/output token be calculated?

CatDesk does not get official token usage numbers from ChatGPT Web. It estimates them locally with `o200k_base`, the same tokenizer family used by GPT-5.4-style models, so the numbers are useful, but still only estimates.

| Field          | Symbol | What it means                | Price                         |
| -------------- | ------ | ---------------------------- | ----------------------------- |
| `inputTokens`  | `↓`    | Tool input ≈ LLM output      | ≈ `$15.00 / 1M` output tokens |
| `outputTokens` | `↑`    | Tool output ≈ LLM input      | ≈ `$2.50 / 1M` input tokens   |
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

Put it in the workspace root. CatDesk reads `AGENTS.md` every time `catdesk_instruction` is called.

# Safety

> [!CAUTION]
> Do **NOT** share the `MCP Server URL` with anyone. Anyone with the URL can access your computer.

The URL is made of these parts:

| Part         | Example                       | What it means                          |
| ------------ | ----------------------------- | -------------------------------------- |
| Public URL   | `https://xxxx.ngrok-free.app` | The temporary ngrok address            |
| Random path  | `/Ab3kL9xQ2pTm7VhC`           | A random per-run path added by CatDesk |
| MCP endpoint | `/mcp`                        | The actual MCP endpoint                |

So the full URL looks like this:

```text
https://xxxx.ngrok-free.app/Ab3kL9xQ2pTm7VhC/mcp
```

The URL changes every time you start CatDesk. ChatGPT Web does not provide an edit button for Custom Connectors, so you need to delete the old connector and create a new one with the new URL.
