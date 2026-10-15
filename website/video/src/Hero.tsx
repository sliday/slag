import React from 'react';
import {AbsoluteFill, Easing, useCurrentFrame, useVideoConfig} from 'remotion';
import {Captions} from './Captions';
import {EndCard} from './EndCard';
import type {HeroScript} from './script';
import {sceneBursts, TerminalScene} from './Terminal';
import {FONT, layout, palette} from './theme';

const ease = Easing.inOut(Easing.cubic);
const ramp = (t: number, from: number, dur: number) => ease(Math.min(1, Math.max(0, (t - from) / dur)));

export const Hero: React.FC<HeroScript> = (script) => {
  const frame = useCurrentFrame();
  const {fps} = useVideoConfig();
  const t = frame / fps;
  const {endCard: card} = script;

  // The window leaves for the end card and comes back for the loop.
  const gone = ramp(t, card.from, 0.3) * (1 - ramp(t, card.to - 0.4, 0.5));
  const win = layout.window;

  // Each passing proof strikes the forge: the glow flares, then cools.
  const strikes = script.scenes.flatMap((s) => sceneBursts(s).map((b) => s.from + b.at));
  const heat = strikes.reduce((acc, at) => (t >= at ? acc + Math.exp(-(t - at) / 0.35) : acc), 0);
  const flare = Math.min(1, heat);

  return (
    <AbsoluteFill style={{background: palette.bg0, fontFamily: FONT, color: palette.fg0}}>
      <AbsoluteFill
        style={{
          background: `radial-gradient(ellipse 70% 55% at 50% 100%, rgba(255, 153, 64, ${0.07 + 0.06 * flare}), rgba(255, 153, 64, 0) 70%)`,
        }}
      />
      <Captions captions={script.captions} t={t} />
      <div
        style={{
          position: 'absolute',
          left: win.x,
          top: win.y,
          width: win.w,
          height: win.h,
          borderRadius: win.radius,
          background: palette.bg1,
          border: `1px solid ${palette.bg3}`,
          // The border carries the edge; the glow only answers a strike.
          boxShadow: `0 0 70px rgba(255, 153, 64, ${0.22 * flare})`,
          overflow: 'hidden',
          opacity: 1 - gone,
          transform: `scale(${1 - 0.015 * gone})`,
        }}
      >
        <div style={{height: win.strip, borderBottom: `1px solid ${palette.bg2}`, display: 'flex', alignItems: 'center', gap: 14, paddingLeft: 26}}>
          {[0, 1, 2].map((i) => (
            <div key={i} style={{width: 14, height: 14, borderRadius: 7, background: palette.bg3}} />
          ))}
        </div>
        <div style={{position: 'absolute', left: 0, top: win.strip, right: 0, bottom: 0}}>
          {script.scenes
            .filter((scene) => t >= scene.from && t < scene.to)
            .map((scene) => (
              <TerminalScene key={scene.id} scene={scene} t={t - scene.from} frame={frame} />
            ))}
        </div>
      </div>
      <EndCard card={card} t={t} />
    </AbsoluteFill>
  );
};
