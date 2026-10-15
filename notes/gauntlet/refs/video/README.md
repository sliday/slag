# Reference launch videos (quality bar)

Warp, Linear, Raycast, Vercel, Cursor pages had no downloadable hero video in HTML (JS-rendered or none). Substituted: charm.sh (2 videos), zed.dev, GitHub Copilot CLI.

| Name | Source | Duration | Resolution | fps | Codec | Size |
|------|--------|----------|-----------|-----|-------|------|
| charm-crush | charm.sh/crush-promo mp4 | 25.9 s | 1920x1080 | 24 | h264 | 14.2 MB |
| charm-wish | charm.sh/wish mp4 | 10.0 s | 800x450 | 30 | h264 | 3.2 MB |
| zed | zed.dev HLS (cloudflarestream, re-encoded, may end early: HLS demux error) | 15.1 s | 1566x1080 | 60 | h264 | 1.6 MB |
| ghcli | github.com/features/copilot/cli mp4 | 6.9 s | 2412x1278 | 60 | h264 | 1.8 MB |

Files per video: `<name>-contact.png` (1 frame/s), `<name>-key-{1,2,3}.png`, `<name>-mobile.png` (390 px).

## charm-crush (CLI product promo, the closest match)
- Pacing: 1-2 s per beat. Title burst 0-4 s, TUI footage 4-17 s with kinetic word cards ("Smarter", "Glamo") every 3-4 s, 3D logo sting 18-26 s.
- Text: huge display type (30-50% of frame) for 1-2 words, on screen 1-2 s. Real TUI text is small and dimmed; it acts as texture, not copy.
- UI: real TUI recorded, then darkened with a bright dialog or diff pulled forward. Purple/green accent on near-black. Ends on logo plate.

## zed (editor product demo)
- Pacing: one 15 s continuous take. A single typed prompt, then diff accept. No cuts.
- Text: zero overlay copy. Only keystroke badges ("Space", Enter) at bottom center, about 1 s each, which show the action.
- UI: full app window, slight tilt and vignette at start. Code is unreadable at thumbnail size; the red/green diff colour block carries the story.

## ghcli (GitHub Copilot CLI)
- Pacing: 7 s. 1 s typed command, 2 s pixel-art logo reveal, then tabs and tip lines. Slow, clean.
- Text: big ASCII logo (about 30% of width) on screen about 2 s; status lines are tiny and legible only on desktop.
- UI: terminal window chrome (3 dots), tab bar, blinking cursor. Command typed live: `copilot`.

## charm-wish (CLI demo, ASCII art)
- Pacing: 10 s. 1 s ASCII figure, 2 s typed `ssh` command, then 6 s colour-cycling ASCII figure (pink, blue, green).
- Text: one command line only, tiny; the ASCII art is the visual. Colour change every 1-2 s keeps motion.
- UI: no chrome, pure terminal on dark navy. Low resolution (800x450) and still reads as polished.

## Top patterns for a CLI hero video
1. Type the real command first (1-2 s), then show output; end on a logo or result plate.
2. A beat every 1-3 s; 7-26 s total. One idea per beat.
3. Big type for 1-2 words max (about 1-2 s), tiny real terminal text as texture. Test at 390 px: only colour blocks and display type survive.
