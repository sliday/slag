import React from 'react';
import {Easing} from 'remotion';
import type {EndCard as EndCardData} from './script';
import {DENSITY, ShadeCell} from './Shade';
import {palette} from './theme';

const ease = Easing.out(Easing.cubic);
const ramp = (t: number, from: number, dur: number) => ease(Math.min(1, Math.max(0, (t - from) / dur)));

// The site logo: heat runs from cold edges to the pure-white name.
const MARK: [string, string][] = [
  ...'░░░'.split('').map((c): [string, string] => [c, palette.cold]),
  ['▒', palette.warm],
  ['▒', palette.hot],
  ['▓', palette.bright],
  ['█', palette.pure],
  [' ', ''],
  [' ', ''],
  ...'SLAG'.split('').map((c): [string, string] => [c, palette.pure]),
  [' ', ''],
  [' ', ''],
  ['█', palette.pure],
  ['▓', palette.bright],
  ['▒', palette.hot],
  ['▒', palette.warm],
  ...'░░░'.split('').map((c): [string, string] => [c, palette.cold]),
];

export const EndCard: React.FC<{card: EndCardData; t: number}> = ({card, t}) => {
  if (t < card.from || t >= card.to) return null;
  const leave = ramp(t, card.to - 0.4, 0.4);
  const n = MARK.length;
  return (
    <div
      style={{
        position: 'absolute',
        inset: 0,
        display: 'flex',
        flexDirection: 'column',
        alignItems: 'center',
        justifyContent: 'center',
        opacity: 1 - leave,
        transform: `scale(${1 - 0.02 * leave})`,
      }}
    >
      <div style={{fontSize: 104, fontWeight: 700, whiteSpace: 'pre', lineHeight: 1.1}}>
        {MARK.map(([ch, color], i) => {
          const d = Math.min(i, n - 1 - i);
          const p = ramp(t, card.from + 0.12 + d * 0.045, 0.2);
          const glow = /[A-Z]/.test(ch);
          return (
            <span
              key={i}
              style={{
                display: 'inline-block',
                color,
                opacity: p,
                transform: `translateY(${(1 - p) * 14}px)`,
                textShadow: glow ? `0 0 ${36 * p}px rgba(255, 153, 64, ${0.55 * p})` : undefined,
              }}
            >
              {ch in DENSITY ? <ShadeCell ch={ch} color={color} height="0.78em" gap="0.1ch" /> : ch}
            </span>
          );
        })}
      </div>
      <div
        style={{
          marginTop: 56,
          fontSize: 60,
          fontWeight: 700,
          color: palette.pure,
          opacity: ramp(t, card.from + 0.75, 0.3),
          transform: `translateY(${(1 - ramp(t, card.from + 0.75, 0.3)) * 16}px)`,
        }}
      >
        {card.tagline}
      </div>
      <div
        style={{
          marginTop: 28,
          fontSize: 54,
          color: palette.bright,
          opacity: ramp(t, card.from + 0.95, 0.3),
          transform: `translateY(${(1 - ramp(t, card.from + 0.95, 0.3)) * 16}px)`,
        }}
      >
        {card.url}
      </div>
    </div>
  );
};
