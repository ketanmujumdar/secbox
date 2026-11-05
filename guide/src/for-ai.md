# Using these docs with an AI

Every page of this site is plain Markdown, so you can give it to an AI assistant without copying from a web page. This page lists the ways to do that.

## On any page

Each page has two buttons at the top right:

- **Copy page** copies the page's Markdown to your clipboard, ready to paste into a chat.
- **Markdown** opens the raw Markdown of the page.

Any page's Markdown is also at the same address with `.md` in place of `.html`. For example:

```text
https://ketanmujumdar.github.io/secbox/guides/secrets.html   the page
https://ketanmujumdar.github.io/secbox/guides/secrets.md     its Markdown
```

## The whole site at once

| File | What it holds |
| --- | --- |
| [`llms.txt`](https://ketanmujumdar.github.io/secbox/llms.txt) | An index of every page with a one-line summary and a link to its Markdown, in the [llms.txt](https://llmstxt.org) format |
| [`llms-full.txt`](https://ketanmujumdar.github.io/secbox/llms-full.txt) | Every page in reading order, in one file, with links made absolute |

Give an assistant `llms-full.txt` when you want it to answer questions about secbox. It is small enough to fit in most chat context windows. Give it `llms.txt` when it can fetch pages itself and you want it to choose what to read.

## From the repository

The pages are in the repository under [`guide/src`](https://github.com/ketanmujumdar/secbox/tree/main/guide/src), so an agent working in a clone can read them directly. The order is in `guide/src/SUMMARY.md`.

## A prompt to start with

```text
Here is the documentation for secbox, a tool that runs a project in a
disposable microVM. Answer only from it. If it says a feature is not built
or not tested, say so plainly rather than guessing.

<paste llms-full.txt here>
```

## Keep in mind

secbox is an early build and these docs say plainly what is not built yet. An assistant can still mix in guesses. Check anything security-related against [What works today](start/status.md).
