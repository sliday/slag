// Palette mirrors the --slag-* and background/foreground variables in
// website/src/main.css, which mirror COLD/WARM/HOT/BRIGHT/PURE in
// slag-rs/src/tui.rs. render.sh refuses to render when they drift.
export const palette = {
  bg0: '#0a0e14',
  bg1: '#13171f',
  bg2: '#1f2430',
  bg3: '#2b3040',
  fg0: '#c7c7c7',
  fg1: '#6b7385',
  fg2: '#4f5b66',
  cold: '#6b7385',
  warm: '#e06c75',
  hot: '#ff9940',
  bright: '#ffd866',
  pure: '#ffffff',
} as const;

export type Tone = 'cold' | 'warm' | 'hot' | 'bright' | 'pure' | 'fg' | 'dim';

export const toneColor = (t: Tone = 'fg'): string => {
  switch (t) {
    case 'cold':
      return palette.cold;
    case 'warm':
      return palette.warm;
    case 'hot':
      return palette.hot;
    case 'bright':
      return palette.bright;
    case 'pure':
      return palette.pure;
    case 'dim':
      return palette.fg2;
    default:
      return palette.fg0;
  }
};

export const FONT = "'Cascadia Code', monospace";

// Sized so the smallest text stays about 13px tall when the video plays
// 390px wide on a phone (54 * 390 / 1600 = 13.2).
export const layout = {
  width: 1600,
  height: 900,
  fps: 30,
  captionX: 96,
  captionY: 64,
  captionSize: 72,
  window: {x: 72, y: 204, w: 1456, h: 664, radius: 16, strip: 52},
  termSize: 54,
  lineHeight: 76,
  padX: 44,
  padTop: 30,
  // Cascadia Code advance width is 1200/2048 em.
  advance: 1200 / 2048,
};

export const charW = layout.termSize * layout.advance;
