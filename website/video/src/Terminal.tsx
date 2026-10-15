import React from 'react';
import {Easing, interpolate, random} from 'remotion';
import {Glyph} from './Glyph';
import {ShadeCell} from './Shade';
import type {Line, RollSeg, Scene, Seg, TextSeg} from './script';
import {charW, layout, palette, toneColor} from './theme';

const ease = Easing.out(Easing.cubic);
const clamp01 = (v: number) => Math.min(1, Math.max(0, v));
const ramp = (t: number, from: number, dur: number) => ease(clamp01((t - from) / dur));

const isText = (s: Seg): s is TextSeg => 'text' in s;
const isRoll = (s: Seg): s is RollSeg => 'roll' in s;

const rollIndex = (s: RollSeg, t: number) =>
  t < s.from ? 0 : Math.min(s.roll.length - 1, Math.floor((t - s.from) / s.every));

const segLength = (s: Seg, t: number): number => {
  if (isText(s)) return s.text.length;
  if (isRoll(s)) return s.roll[rollIndex(s, t)].length;
  return 1;
};

// Column of the first glyph cell, where sparks start.
const sparkColumn = (segs: Seg[], t: number): number => {
  let col = 0;
  for (const s of segs) {
    if ('glyph' in s) return col;
    if (isText(s) && /[█▓▒░]/.test(s.text)) return col + s.text.search(/[█▓▒░]/);
    col += segLength(s, t);
  }
  return 2;
};

const currentSegs = (line: Line, t: number): Seg[] => {
  let segs = line.segs;
  for (const s of line.then ?? []) if (t >= s.at) segs = s.segs;
  return segs;
};

// The window holds `layout.rows` rows. A line past the last row scrolls
// the page up, the way a terminal does, over a short glide.
export const scrollAt = (scene: Scene, t: number): number => {
  const steps = scene.lines
    .map((l) => ({at: l.at, top: Math.max(0, l.row - (layout.rows - 1))}))
    .sort((a, b) => a.at - b.at);
  let prev = 0;
  let cur = 0;
  let at = 0;
  for (const step of steps) {
    if (step.at > t) break;
    if (step.top > cur) {
      prev = cur;
      cur = step.top;
      at = step.at;
    }
  }
  return prev + (cur - prev) * ramp(t, at, 0.16);
};

export type Burst = {at: number; x: number; y: number; seed: string; big: boolean};

export const sceneBursts = (scene: Scene): Burst[] => {
  const out: Burst[] = [];
  for (const line of scene.lines) {
    const push = (at: number, segs: Seg[]) => {
      const y = layout.padTop + (line.row - scrollAt(scene, at + 0.16)) * layout.lineHeight + layout.lineHeight / 2;
      const x = layout.padX + (sparkColumn(segs, at) + 0.5) * charW;
      out.push({at, x, y, seed: `${scene.id}-${line.row}-${at}`, big: segs.some((s) => 'glyph' in s && s.glyph === 'ok')});
    };
    if (line.burst) push(line.at, line.segs);
    for (const s of line.then ?? []) if (s.burst) push(s.at, s.segs);
  }
  return out;
};

const Roll: React.FC<{seg: RollSeg; t: number; color: string}> = ({seg, t, color}) => {
  const i = rollIndex(seg, t);
  const word = seg.roll[i];
  const since = t - (seg.from + i * seg.every);
  const p = seg.slide && i > 0 && since < 0.14 ? ease(clamp01(since / 0.14)) : 1;
  if (p >= 1) return <span style={{color}}>{word}</span>;
  const prev = seg.roll[i - 1];
  const h = layout.lineHeight;
  return (
    // clip-path, not overflow: an overflow-hidden inline-block moves its
    // baseline to the bottom edge and drops the word below the line.
    <span style={{display: 'inline-block', position: 'relative', clipPath: 'inset(0 -0.2ch)'}}>
      <span style={{visibility: 'hidden'}}>{word.length >= prev.length ? word : prev}</span>
      <span style={{position: 'absolute', left: 0, top: -h * p, color, opacity: 1 - p}}>{prev}</span>
      <span style={{position: 'absolute', left: 0, top: h * (1 - p), color}}>{word}</span>
    </span>
  );
};

// Plain text, with the shade characters drawn as flat cells.
const TextRun: React.FC<{text: string; color: string; bold?: boolean; opacity: number}> = ({text, color, bold, opacity}) => {
  const parts = text.split(/([░▒▓█])/).filter((p) => p !== '');
  return (
    <span style={{color, fontWeight: bold ? 700 : 400, opacity}}>
      {parts.map((p, i) => (/^[░▒▓█]$/.test(p) ? <ShadeCell key={i} ch={p} color={color} height="0.72em" gap="0.16ch" /> : p))}
    </span>
  );
};

const LineView: React.FC<{line: Line; t: number; frame: number; top: number; exit?: Scene['exit']}> = ({line, t, frame, top, exit}) => {
  if (t < line.at) return null;
  const segs = currentSegs(line, t);
  const total = segs.reduce((a, s) => a + segLength(s, t), 0);
  const visible = line.type
    ? Math.min(total, line.type.keep + Math.max(0, Math.floor((t - line.type.at) * line.type.cps)))
    : total;

  let opacity = line.instant ? 1 : ramp(t, line.at, 0.22);
  let y = line.instant ? 0 : (1 - opacity) * 12;
  if (exit && t >= exit.at && !(exit.keep ?? []).includes(line.row)) {
    const q = ramp(t, exit.at, 0.25);
    opacity *= 1 - q;
    y -= q * 14;
  }
  const dim = line.dimAt !== undefined ? 1 - 0.6 * ramp(t, line.dimAt, 0.3) : 1;

  const nodes: React.ReactNode[] = [];
  let used = 0;
  segs.forEach((s, k) => {
    if (used >= visible) return;
    const len = segLength(s, t);
    const room = visible - used;
    used += Math.min(len, room);
    if ('glyph' in s) {
      nodes.push(
        <span key={k} style={{opacity: dim}}>
          <Glyph name={s.glyph} color={toneColor(s.tone)} t={t} />
        </span>,
      );
      return;
    }
    if (isRoll(s)) {
      const word = s.roll[rollIndex(s, t)];
      nodes.push(
        <span key={k} style={{opacity: dim}}>
          {room >= len ? <Roll seg={s} t={t} color={toneColor(s.tone)} /> : <span style={{color: toneColor(s.tone)}}>{word.slice(0, room)}</span>}
        </span>,
      );
      return;
    }
    const text = s.text.slice(0, room);
    const lit = s.lit?.find((l) => t >= l.from && t < l.to);
    if (lit) {
      const name = text.trimEnd();
      const on = ramp(t, lit.from, 0.1);
      nodes.push(
        <span key={k}>
          <span
            style={{
              color: toneColor(lit.tone),
              background: `rgba(255, 153, 64, ${0.16 * on})`,
              boxShadow: `0 0 0 0.2ch rgba(255, 153, 64, ${0.16 * on})`,
              borderRadius: 6,
            }}
          >
            {name}
          </span>
          {text.slice(name.length)}
        </span>,
      );
      return;
    }
    nodes.push(<TextRun key={k} text={text} color={toneColor(s.tone)} bold={s.bold} opacity={dim} />);
  });

  const c = line.cursor;
  if (c && t >= c.from && t < c.to) {
    const typing = line.type !== undefined && t >= line.type.at && visible < total;
    // Blink on the absolute frame, 15 on and 15 off. 720 frames divide
    // evenly, so the blink runs straight through the loop point.
    const on = typing || Math.floor(frame / 15) % 2 === 0;
    nodes.push(
      <span
        key="cursor"
        style={{
          display: 'inline-block',
          width: '0.62ch',
          height: '1.08em',
          marginLeft: '0.08ch',
          verticalAlign: '-0.2em',
          background: palette.fg0,
          opacity: on ? 0.9 * dim : 0,
        }}
      />,
    );
  }

  return (
    <div
      style={{
        position: 'absolute',
        left: layout.padX,
        top: layout.padTop + (line.row - top) * layout.lineHeight,
        height: layout.lineHeight,
        lineHeight: `${layout.lineHeight}px`,
        whiteSpace: 'pre',
        opacity,
        transform: `translateY(${y}px)`,
      }}
    >
      {nodes}
    </div>
  );
};

const SPARK_COLORS = [palette.bright, palette.hot, palette.pure, palette.bright, palette.hot];

export const Sparks: React.FC<{bursts: Burst[]; t: number}> = ({bursts, t}) => {
  const out: React.ReactNode[] = [];
  for (const b of bursts) {
    const tau0 = t - b.at;
    if (tau0 < 0 || tau0 > 1) continue;
    const n = b.big ? 16 : 10;
    for (let i = 0; i < n; i++) {
      const r = (k: string) => random(`${b.seed}-${i}-${k}`);
      const life = 0.3 + r('life') * 0.25;
      const tau = tau0 - r('delay') * 0.05;
      if (tau < 0 || tau > life) continue;
      // Into the empty gutter left of the glyph column and up, never
      // across the text to its right.
      const angle = -Math.PI * (0.62 + r('angle') * 0.36);
      const speed = (b.big ? 260 : 200) + r('speed') * 260;
      const x = b.x + Math.cos(angle) * speed * tau;
      const y = b.y + Math.sin(angle) * speed * tau + 0.5 * 1500 * tau * tau;
      const size = 4 + Math.round(r('size') * 3);
      const fade = interpolate(tau, [0, life * 0.5, life], [1, 0.85, 0], {extrapolateRight: 'clamp'});
      const color = SPARK_COLORS[Math.floor(r('color') * SPARK_COLORS.length)];
      out.push(
        <div
          key={`${b.seed}-${i}`}
          style={{
            position: 'absolute',
            left: x - size / 2,
            top: y - size / 2,
            width: size,
            height: size,
            background: color,
            opacity: fade,
            boxShadow: `0 0 ${size * 2}px ${color}`,
          }}
        />,
      );
    }
  }
  return <>{out}</>;
};

const FocusBand: React.FC<{scene: Scene; t: number; top: number}> = ({scene, t, top}) => {
  const marks = scene.focus ?? [];
  let i = -1;
  marks.forEach((m, k) => {
    if (t >= m.at) i = k;
  });
  if (i < 0) return null;
  const rowY = (row: number) => layout.padTop + (row - top) * layout.lineHeight;
  const from = i > 0 ? rowY(marks[i - 1].row) : rowY(marks[i].row);
  const y = from + (rowY(marks[i].row) - from) * ramp(t, marks[i].at, 0.28);
  let opacity = ramp(t, marks[0].at, 0.25);
  if (scene.exit && t >= scene.exit.at) opacity *= 1 - ramp(t, scene.exit.at, 0.2);
  return (
    <div
      style={{
        position: 'absolute',
        left: 18,
        right: 18,
        top: y + 4,
        height: layout.lineHeight - 8,
        borderRadius: 10,
        background: 'rgba(255, 255, 255, 0.06)',
        opacity,
      }}
    />
  );
};

export const TerminalScene: React.FC<{scene: Scene; t: number; frame: number}> = ({scene, t, frame}) => {
  const bursts = sceneBursts(scene);
  const top = scrollAt(scene, t);
  return (
    // Clip at the first row, as a terminal does: a scrolled row never
    // peeks into the top padding.
    <div style={{position: 'absolute', inset: 0, fontSize: layout.termSize, overflow: 'hidden', clipPath: `inset(${layout.padTop}px 0 0 0)`}}>
      <FocusBand scene={scene} t={t} top={top} />
      {scene.lines.map((line) => (
        <LineView key={`${scene.id}-${line.row}`} line={line} t={t} frame={frame} top={top} exit={scene.exit} />
      ))}
      <Sparks bursts={bursts} t={t} />
    </div>
  );
};
