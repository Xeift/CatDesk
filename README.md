# CatDesk

An open-source tool that turns ChatGPT Web into a coding agent. No reverse engineering, no API, no Codex. A ChatGPT Plus subscription is enough.

# Disclaimer

This is an independent open-source project and is not affiliated with or endorsed by OpenAI. I built it as a personal tool and decided to open-source it. Some features are still buggy and may cause unexpected behavior. Use it at your own risk. I am not responsible for any loss caused by this tool. It is strongly recommended to run it inside a VM or container.

# Why CatDesk?

Codex has a very generous weekly quota (2x usage + reset usage frequently) compared to Antigravity and Claude Code (3 Opus prompts then 5h quota is gone lol), that's why I love OpenAI so much.

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

That's the idea behind CatDesk! It gives ChatGPT Web tools like `write_file` and `run_commands` to edit files on your computer.

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

I tried this with GPT-5.2 before, and the results were poor. However, **GPT-5.4 Thinking is now really good at tool calling and computer use.** The first time I tried it with GPT-5.4, I was honestly surprised by how well it worked.

# Differences between ChatGPT + CatDesk, Codex, and the API (let's say Plus plan)

|       | ChatGPT + CatDesk                                  | Codex                   | OpenAI API           |
| ----- | -------------------------------------------------- | ----------------------- | -------------------- |
| Usage | 3,000 messages/week                                | Generous weekly quota   | Pay as you go        |
| Pros  | Stable, no extra fee, and nearly unlimited\* quota | Stable and no extra fee | Stable               |
| Cons  | Not as smooth as native Codex                      | Runs out very quickly   | Tokens are expensive |

\*Let's say you sleep 6 hours a day and use CatDesk every day. In that case, you can send 3,000 / (24 - 6) / 7 = 23.8 messages per hour. Since thinking and tool calls take time, it is very difficult to use up your weekly 3,000 message limit.

# Who needs this?

- People who used up their Codex quota on the first day after it reset (me🥺)
- People who are working on web development and crawlers. (CatDesk enables ChatGPT Web to read elements and control your browser tab through chrome-devtools-mcp integration.)

# Quickstart

> [!CAUTION]
> This tool is very powerful and can potentially wipe your whole disk or produce unexpected results.
> Run it inside a VM or container (DevContainer is a good option).
> Treat it like OpenClaw, keep it containerized and isolated.

1. Download the CatDesk binary for your platform.
   Put it anywhere you want. If your platform requires it, make the binary executable first.

2. Run the CatDesk binary.

   ```bash
   ./catdesk
   ```

   By default, CatDesk listens on port `3200`, as defined in [`main()`](/home/xeift/Desktop/MCP3000/src/main.rs#L325). You can override it with `PORT`. The workspace root defaults to the current working directory and can be overridden with `WORKSPACE_ROOT`, also in [`main()`](/home/xeift/Desktop/MCP3000/src/main.rs#L325).

   On macOS Terminal.app, CatDesk manages a dedicated `CatDesk` Terminal profile automatically. If the current Terminal tab is not already using that profile, CatDesk applies it, closes any temporary helper window, and asks you to run the same command again in that tab. It only starts immediately when the current tab is already using `CatDesk`. Set `CATDESK_SKIP_MACOS_TERMINAL_PROFILE=1` if you want to keep the current Terminal session untouched.

3. Wait for the TUI to show the MCP Server URL.
   CatDesk starts the local server and ngrok automatically in [`start_services()`](/home/xeift/Desktop/MCP3000/src/main.rs#L1425). The MCP URL is shown in the UI at [`draw_ui()`](/home/xeift/Desktop/MCP3000/src/main.rs#L1748), and the actual value includes a random runtime slug, generated in [`generate_mcp_slug()`](/home/xeift/Desktop/MCP3000/src/state.rs#L501). Because ngrok URLs and the slug both change between runs, you should treat the URL as ephemeral.

4. Open [ChatGPT connector settings](https://chatgpt.com/apps#settings/Connectors).

5. Click `Create app`.

6. Fill in the connector form.
   Use:
   - Name: `CatDesk` or any name you like
   - MCP Server URL: the full URL shown by CatDesk
   - Authentication: `None`

   These values come directly from the in-app guide in [`guide_lines`](/home/xeift/Desktop/MCP3000/src/main.rs#L2150).

7. Click `I understand and want to continue`.

8. Click `Create`.

9. Add this to your ChatGPT system instruction:

```text
CatDesk is a coding tool and a custom connector. Always use CatDesk if the user wants to do anything related to file operations. Always call `catdesk_instruction` after `list_resources`, and follow the instructions it contains.
```

10. Start using the connector from ChatGPT Web. Some important tips:

- I strongly recommend manually selecting the connector using `/` or `@`. This way, ChatGPT can only access the connector you selected, which may improve stability.
- **Do NOT let ChatGPT automatically decide which connector to use.** There is a strange bug where ChatGPT keeps saying _"Resource not found."_ This bug is driving me crazy. It is also very unstable if you do not explicitly select CatDesk.
- However, the `web` tool that ChatGPT uses to search and open links will be disabled if you explicitly select CatDesk. The `web` tool and a custom connector cannot be used at the same time. If you need to search some docs, click the x to remove CatDesk temporarily. After finishing the search, add it back.

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

# Tools

CatDesk's tools are:

| Tool                  | Type  | What it does                                                            |
| --------------------- | ----- | ----------------------------------------------------------------------- |
| `catdesk_instruction` | Guide | Returns CatDesk usage instructions and reads your workspace `AGENTS.md` |
| `read_file`           | Read  | Reads a text file from the workspace                                    |
| `search_text`         | Read  | Searches text across files in the workspace                             |
| `write_file`          | Write | Creates or overwrites a file                                            |
| `append_file`         | Write | Appends text to a file, or creates it if missing                        |
| `move_path`           | Write | Moves or renames a file or directory                                    |
| `delete_path`         | Write | Deletes a file or directory                                             |
| `replace_in_file`     | Write | Replaces text inside a file                                             |
| `run_command`         | Shell | Runs a shell command inside the workspace.                              |

If browser mode is enabled, CatDesk can also expose extra browser/devtools tools. Those are provided by the browser bridge, so the exact list depends on your environment.

# Context window

According to [the blog](<https://help.openai.com/en/articles/11909943-gpt-53-and-gpt-54-in-chatgpt#:~:text=Thinking%20(GPT%E2%80%915.4%20Thinking)>) and [the code](https://github.com/openai/codex/blob/main/codex-rs/models-manager/src/model_info.rs#L85), the context window in ChatGPT web is different from Codex.

| Tier     | CatDesk + ChatGPT Web (in + out = sum) | Codex CLI (sum)        |
| -------- | -------------------------------------- | ---------------------- |
| Plus     | 128K + 128K = 256K                     | 258K (1M experimental) |
| Pro tier | 272K + 128K = 400K                     | 258K (1M experimental) |

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

### Can I skip approval, like with `--yolo` or `--dangerously-skip-permissions`?

<p align="center">
  <img src="docs/images/approval.png" alt="Approval required for sensitive operations" width="500"><br>
  <em>Approval required for sensitive operations</em>
</p>

No. This restriction comes from the ChatGPT Web side. There is not much CatDesk can do about it. ChatGPT Web probably uses an LLM or some internal policy layer to detect high-risk operations and require manual approval. Sensitive filenames and sensitive content can both trigger manual approval. It is not only about which tool is being used. For example:

> write_file("api_key.txt", content: "")<br>
> ⚠️ Approval required

> write_file("Xeift.txt", content: "api_key")<br>
> ⚠️ Approval required

> write_file("i_luv_catgirl.txt", content: "")<br>
> ✅ No approval required

Sometimes this is annoying, but there is no good workaround right now. This is one of the reasons I say CatDesk is _not as smooth as native Codex_.

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

Put it in the workspace root. If not set, will use `~/.codex/AGENTS.md`. CatDesk reads `AGENTS.md` every time `catdesk_instruction` is called.

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

The URL changes every time you start CatDesk (both `Public URL` and `Random path`). ChatGPT Web does not provide an edit button for Custom Connectors, so you need to delete the old connector and create a new one with the new URL.

# About Binagotchy

<p align="center">
  <img src="docs/images/binagotchy.gif" alt="Binagotchy!" width="500"><br>
  <em>Binagotchy!</em>
</p>

The character is a cute shark-cat! I actually made this before CatDesk and decided to put it in the project.

By default, CatDesk will generate a random Binagotchy every time you start it. If you see a cute one, you can set it as your partner on the launch screen. The system will also automatically save every Binagotchy in `~/.catdesk/binagotchy`. You can download it too (or, to be accurate, export it)! Both `.png` and `.gif` are supported. Feel free to use it anywhere. This project and Binagotchy are both under the MIT License. By the way, Binagotchy is generated using pure scripts and does not use any text-to-image or diffusion model.
