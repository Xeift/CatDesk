# CatDesk

MCP Tools for ChatGPT Web to control your computer and browser. No API, no Codex. Only ChatGPT Plus subscription is required. Just use your 3000 weekly Thinking messages in ChatGPT Web.

# Who needs this?

- People who used up their Codex quota on the first day after it reset (me🥺)
- People who are working on web development and crawlers. (CatDesk enables ChatGPT Web to read elements and control your browser tab through chrome-devtools-mcp integration.)

# How does this work?

1. A ChatGPT Plus or Pro subscription is required.
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
> This tool is very powerful and can potentially wipe your whole disk.
> Run it inside a VM or container (DevContainer is a good option).
> Treat it like OpenClaw, keep it containerized and isolated.
