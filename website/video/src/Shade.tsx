import React from 'react';

// The shade characters ░ ▒ ▓ █ as flat cells. Cascadia draws them as
// diagonal hatching, which reads as noise at video sizes and costs
// bitrate; the alpha carries the same density instead.
export const DENSITY: Record<string, number> = {'░': 0.25, '▒': 0.5, '▓': 0.75, '█': 1};

export const ShadeCell: React.FC<{ch: string; color: string; height?: string; gap?: string; opacity?: number}> = ({
  ch,
  color,
  height = '0.62em',
  gap = '0.12ch',
  opacity = 1,
}) => (
  <span style={{display: 'inline-block', width: '1ch', verticalAlign: 'middle', lineHeight: 0}}>
    <span
      style={{
        display: 'block',
        width: `calc(1ch - ${gap})`,
        height,
        background: color,
        opacity: (DENSITY[ch] ?? 1) * opacity,
        borderRadius: 3,
      }}
    />
  </span>
);
