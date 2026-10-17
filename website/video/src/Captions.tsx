import React from 'react';
import {Easing} from 'remotion';
import type {Caption} from './script';
import {layout, palette} from './theme';

const ease = Easing.out(Easing.cubic);
const ramp = (t: number, from: number, dur: number) => ease(Math.min(1, Math.max(0, (t - from) / dur)));

type Word = {text: string; accent: boolean};

const words = (c: Caption): Word[] => {
  const start = c.accent ? c.text.indexOf(c.accent) : -1;
  const end = start + (c.accent?.length ?? 0);
  const out: Word[] = [];
  let i = 0;
  for (const w of c.text.split(' ')) {
    out.push({text: w, accent: start >= 0 && i < end && i + w.length > start});
    i += w.length + 1;
  }
  return out;
};

const Line: React.FC<{c: Caption; t: number; leaving: number}> = ({c, t, leaving}) => {
  const entering = c.at > 0;
  return (
    <div
      style={{
        position: 'absolute',
        left: layout.captionX,
        top: layout.captionY,
        whiteSpace: 'pre',
        fontSize: layout.captionSize,
        fontWeight: 700,
        lineHeight: 1.2,
        letterSpacing: '-0.01em',
        opacity: 1 - leaving,
        transform: `translateY(${-14 * leaving}px)`,
      }}
    >
      {words(c).map((w, i) => {
        const p = entering ? ramp(t, c.at + 0.08 + i * 0.035, 0.3) : 1;
        return (
          <span
            key={i}
            style={{
              display: 'inline-block',
              marginRight: '1ch',
              color: w.accent ? palette.hot : palette.pure,
              opacity: p,
              transform: `translateY(${(1 - p) * 20}px)`,
            }}
          >
            {w.text}
          </span>
        );
      })}
    </div>
  );
};

export const Captions: React.FC<{captions: Caption[]; t: number}> = ({captions, t}) => {
  let i = -1;
  captions.forEach((c, k) => {
    if (t >= c.at) i = k;
  });
  if (i < 0) return null;
  const cur = captions[i];
  const prev = i > 0 ? captions[i - 1] : undefined;
  const out = ramp(t, cur.at, 0.16);
  return (
    <>
      {prev && prev.text && out < 1 ? <Line c={prev} t={t} leaving={out} /> : null}
      {cur.text ? <Line c={cur} t={t} leaving={0} /> : null}
    </>
  );
};
