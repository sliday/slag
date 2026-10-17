import React from 'react';
import type {Glyph as GlyphName} from './script';

// Cascadia Code's web subsets carry the block elements but not these
// four, so they are drawn to fill exactly one monospace cell.
export const Glyph: React.FC<{name: GlyphName; color: string; t: number}> = ({name, color, t}) => {
  const box: React.CSSProperties = {
    display: 'inline-block',
    width: '1ch',
    height: '1em',
    verticalAlign: '-0.14em',
    position: 'relative',
  };
  const svg: React.CSSProperties = {position: 'absolute', left: '50%', top: '50%', width: '0.7em', height: '0.7em', transform: 'translate(-50%, -50%)', overflow: 'visible'};
  const sw = 2.9;
  let body: React.ReactNode = null;
  if (name === 'ok') {
    body = <path d="M2 10.5 L7.5 16 L18 4.5" fill="none" stroke={color} strokeWidth={sw} strokeLinecap="round" strokeLinejoin="round" />;
  } else if (name === 'fail') {
    body = (
      <g stroke={color} strokeWidth={sw} strokeLinecap="round">
        <path d="M3.5 3.5 L16.5 16.5" />
        <path d="M16.5 3.5 L3.5 16.5" />
      </g>
    );
  } else if (name === 'arrow') {
    body = (
      <g fill="none" stroke={color} strokeWidth={sw} strokeLinecap="round" strokeLinejoin="round">
        <path d="M1.5 10 L18 10" />
        <path d="M12 4 L18 10 L12 16" />
      </g>
    );
  } else if (name === 'hammer') {
    // ⚒, the attempt and anvil glyph: two crossed tools.
    body = (
      <g fill="none" stroke={color} strokeWidth={2.4} strokeLinecap="round" strokeLinejoin="round">
        <path d="M4 17 L14 7" />
        <path d="M16 17 L6 7" />
        <path d="M11 4.5 L16.5 10 L18.5 8 L13 2.5 Z" fill={color} />
        <path d="M9 4.5 L3.5 10 L1.5 8 L7 2.5 Z" fill={color} />
      </g>
    );
  } else if (name === 'ingot') {
    // ▣, an ingot on the anvil.
    body = (
      <g>
        <rect x="3" y="3" width="14" height="14" fill="none" stroke={color} strokeWidth={2} />
        <rect x="7" y="7" width="6" height="6" fill={color} />
      </g>
    );
  } else if (name === 'diamond') {
    // ◆, a finished artifact: the blueprint, the mold.
    body = <path d="M10 2.5 L17.5 10 L10 17.5 L2.5 10 Z" fill={color} />;
  } else if (name === 'resmelt') {
    // ♻, a cracked ingot rewritten and cast again: a closing loop.
    body = (
      <g fill="none" stroke={color} strokeWidth={2.4} strokeLinecap="round" strokeLinejoin="round">
        <path d="M16.5 7.5 A7 7 0 1 0 17 12.5" />
        <path d="M17.5 2.5 L16.8 7.8 L11.6 7" />
      </g>
    );
  } else if (name === 'square') {
    // ■, the session's closing summary.
    body = <rect x="4" y="4" width="12" height="12" fill={color} />;
  } else if (name === 'ore' || name === 'molten' || name === 'forged') {
    // Ingot states as one square: ore is cold and hollow (░), molten
    // glows (▓), forged is solid white (█).
    const pulse = 0.75 + 0.25 * Math.sin(t * 9);
    body =
      name === 'ore' ? (
        <rect x="3.5" y="3.5" width="13" height="13" rx="2" fill={color} fillOpacity={0.25} stroke={color} strokeWidth={2} />
      ) : (
        <rect
          x="3" y="3" width="14" height="14" rx="2" fill={color}
          style={name === 'molten' ? {filter: `drop-shadow(0 0 ${5 * pulse}px ${color})`} : undefined}
        />
      );
  } else {
    // The stream-mode spinner: ◐ ◓ ◑ ◒, one step every 150 ms.
    const step = Math.floor(t / 0.15) % 4;
    body = (
      <g transform={`rotate(${step * 90} 10 10)`}>
        <circle cx="10" cy="10" r="8" fill="none" stroke={color} strokeWidth={2} />
        <path d="M10 2 A8 8 0 0 0 10 18 Z" fill={color} />
      </g>
    );
  }
  return (
    <span style={box}>
      <svg viewBox="0 0 20 20" style={svg}>
        {body}
      </svg>
    </span>
  );
};
