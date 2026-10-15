Complexity: hard — motion design and programmatic video with hard size and legibility budgets, embedded into an existing site.

# Lane: Remotion hero video for slag.dev

Working root: /Users/stas/Playground/slag/.claude/worktrees/lane-video (git worktree, branch gauntlet/video). Work only here, with absolute paths.

Use the `remotion-best-practices` skill for Remotion, and the `impeccable` skill for the site embedding and any copy or layout you touch. `video-toolkit` is available if useful.

## Context

slag (repo root `CLAUDE.md` explains it) is a Rust coding-agent orchestrator with a forge metaphor: a commission becomes S-expression "ingots" that get forged by agents, each checked by a `:proof` shell command, retried with heat, re-smelted when cracked, and judged by a fresh "warden" against a bar. The site `website/` is vanilla Vite (no framework): `index.html`, `src/main.css` (`--slag-*` fire palette, Cascadia Code), `src/content.js`, `src/main.js`. Deployed to Cloudflare Pages as slag.dev. `website/public/install.sh` is a copy of `slag-rs/install.sh`; do not change either.

Positioning the owner wants now: **a fast, LLM-provider-agnostic harness for solving issues.** Two flows are being finished in parallel by other builders:
- `slag fix <issue>` (free text, `#123`, or a GitHub issue URL): reproduces the bug, fixes it, proves it with the repo's own tests, commits on a branch, `--pr` opens a draft PR.
- Any provider with one setting: `--provider cerebras|groq|openai|anthropic|gemini|openrouter|ollama|...`, including mixed providers per role.
- The classic commission flow: `slag "build X"` → ingots → forge → proof → warden → assay.
Show these truthfully. Do not invent numbers (no fake benchmarks, no fake star counts). Use command lines and terminal output that the real CLI would print; when unsure of exact output, keep it generic and plausible, and list each such guess in your report so it can be checked.

## Goal

A short hero video that makes a developer understand what slag does and want to try it within the first 8 seconds, embedded in the slag.dev hero, at the craft level of the best developer-tool launch videos (Warp, Zed, Linear, Raycast, Vercel).

## Deliverables

1. A Remotion project in `website/video/` with its own `package.json` (pin exact versions), kept out of the Vite build. Compositions parameterized so terminal scenes are data, not hard-coded pixels, so they can be updated when the real TUI changes (a TUI builder is redesigning it now; the stream-mode palette lives in `slag-rs/src/tui.rs` and mirrors `website/src/main.css`).
2. Renders committed to `website/public/media/`: `slag-hero.webm` (VP9 or AV1) and `slag-hero.mp4` (H.264, `faststart`), plus `slag-hero-poster.webp` (or .jpg) taken from the strongest frame. Budgets: duration 15 to 30 s; 1920x1080 or 1600x900 at 30 fps (60 only if motion needs it); webm ≤ 3 MB, mp4 ≤ 4 MB; loops without a visible jump; no audio track.
3. Legibility: every piece of text must be readable when the video is shown 390 px wide on a phone. Check by downscaling frames to 390 px wide and reading them. Prefer fewer, larger words.
4. Embed in the hero of `website/index.html`: `<video autoplay muted loop playsinline preload="metadata" poster=...>` with both sources, explicit width/height or aspect-ratio to avoid layout shift, a caption or `aria-label`, and `prefers-reduced-motion: reduce` showing the poster only (no autoplay). It must not hurt the page: lazy behavior where it helps, no blocking JS.
5. Update the site copy that the new positioning makes wrong (hero line, the usage section) so it matches: fast, provider-agnostic, `slag fix`. Keep the rest of the site intact. `cd website && npm run build` must pass.
6. A render script `website/video/render.sh` that renders all outputs, re-encodes to the budgets with ffmpeg, extracts the poster, and writes a contact sheet (`website/video/out/contact.png`, one frame per second, tiled) plus a 390 px-wide copy of the contact sheet. Keep `out/` gitignored except nothing else.

## Paths you own (edit nothing else)

`website/**` except `website/public/install.sh` and `website/public/slag.sh`.

Do NOT touch `slag-rs/`, `bench/`, root docs.

## Hard rules

- No external network assets at runtime on the site (fonts and media are self-hosted or bundled).
- Prose and on-screen copy: no em dashes, active voice, short sentences, no hype words ("revolutionary", "supercharge", "seamless", "unleash").
- Commit on branch `gauntlet/video` in your worktree with messages like `website: one sentence` or `video: one sentence`. Commit the rendered media. Do not push. Do not deploy.

## Report (≤300 words)

Files changed; render command; output sizes and durations (measured with ffprobe); the storyboard beat by beat with timestamps; the contact sheet path; every on-screen claim or CLI output you guessed; known gaps.
