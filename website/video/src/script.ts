import type {Tone} from './theme';

// The whole video is this file. Captions use absolute seconds. Terminal
// lines use seconds from the start of their scene. When the real CLI
// output changes, edit the strings here and run ./render.sh.

export type Glyph = 'ok' | 'fail' | 'spin' | 'arrow' | 'ore' | 'molten' | 'forged';

export type Lit = {from: number; to: number; tone: Tone};

export type TextSeg = {text: string; tone?: Tone; bold?: boolean; lit?: Lit[]};
export type GlyphSeg = {glyph: Glyph; tone?: Tone};
// A word that rolls through `words`, one every `every` seconds from `from`.
export type RollSeg = {roll: string[]; every: number; from: number; tone?: Tone};
// The assay temper bar, filled by the ingots forged so far.
export type BarSeg = {bar: {total: number; forgedAt: number[]}};
export type Seg = TextSeg | GlyphSeg | RollSeg | BarSeg;

export type LineState = {at: number; segs: Seg[]; burst?: boolean};

export type Line = {
  row: number;
  at: number;
  segs: Seg[];
  // Typewriter: `keep` characters show at once, the rest type at `cps`.
  type?: {at: number; cps: number; keep: number};
  // Block cursor after the last visible character between these times.
  cursor?: {from: number; to: number};
  then?: LineState[];
  // Fade to the dim tone at this time (scene-relative).
  dimAt?: number;
  // Appear without the entrance motion (continues a line from the
  // previous scene).
  instant?: boolean;
  // Throw sparks from the first glyph when the line appears.
  burst?: boolean;
};

export type Scene = {
  id: string;
  from: number;
  to: number;
  lines: Line[];
  // Lines leave at this scene-relative time, except rows in `keep`.
  exit?: {at: number; keep?: number[]};
  // A soft band moves to the row that matters at each time.
  focus?: {at: number; row: number}[];
};

export type Caption = {at: number; text: string; accent?: string};

export type EndCard = {from: number; to: number; tagline: string; url: string};

export type HeroScript = {
  duration: number;
  captions: Caption[];
  scenes: Scene[];
  endCard: EndCard;
};

const t = (text: string, tone?: Tone, bold?: boolean): TextSeg => ({text, tone, bold});
const g = (glyph: Glyph, tone?: Tone): GlyphSeg => ({glyph, tone});

const PROMPT = t('$ ', 'cold');
const FIX_CMD = [PROMPT, t('slag', 'bright'), t(' fix 482 ', 'pure'), t('--pr', 'fg')];

// One provider per grid cell; the roll walks the same list.
export const PROVIDERS = [
  'openrouter',
  'cerebras',
  'groq',
  'openai',
  'anthropic',
  'gemini',
  'ollama',
  'xai',
  'mistral',
];
const ROLL = ['groq', 'cerebras', 'anthropic', 'openai', 'gemini', 'ollama', 'mistral', 'xai', 'openrouter'];
const ROLL_FROM = 0.95;
const ROLL_EVERY = 0.27;
const MIX_AT = 3.4;

const cell = (name: string): TextSeg => {
  const i = ROLL.indexOf(name);
  const lit: Lit[] = [];
  if (i >= 0) {
    const from = ROLL_FROM + i * ROLL_EVERY;
    lit.push({from, to: i === ROLL.length - 1 ? MIX_AT : from + ROLL_EVERY, tone: 'hot'});
  }
  if (name === 'anthropic' || name === 'cerebras') {
    lit.push({from: MIX_AT, to: 99, tone: 'hot'});
  }
  return {text: name.padEnd(12), tone: 'fg', lit};
};

const gridRow = (row: number, names: string[], at: number): Line => ({
  row,
  at,
  segs: [t('  '), ...names.map(cell)],
  dimAt: MIX_AT,
});

// Ingot rows: glyph, id, work, status. Widths match across rows.
const ingot = (id: string, work: string, glyph: Seg, status: string, tone: Tone): Seg[] => [
  t('  '),
  glyph,
  t(` [${id}] `, 'cold'),
  t(work.padEnd(17), 'fg'),
  t(status, tone),
];
const ore = (id: string, work: string) => ingot(id, work, g('ore', 'cold'), 'ore', 'cold');
const molten = (id: string, work: string) => ingot(id, work, g('molten', 'hot'), 'molten', 'hot');
const forged = (id: string, work: string) => ingot(id, work, g('forged', 'pure'), 'forged', 'pure');

const INGOTS: [string, string][] = [
  ['i1', 'scaffold the api'],
  ['i2', 'todo model'],
  ['i3', 'crud routes'],
  ['i4', 'auth tokens'],
];
// Scene-relative times in the forge scene.
const FORGED_AT = [2.6, 3.4, 4.5, 4.65];

export const heroScript: HeroScript = {
  duration: 24,
  captions: [
    {at: 0, text: 'Point it at an issue.', accent: 'issue'},
    {at: 1.9, text: 'It reproduces the bug.', accent: 'reproduces'},
    {at: 3.4, text: 'Fixes it. Your tests prove it.', accent: 'tests'},
    {at: 5.5, text: 'You review a draft PR.', accent: 'draft PR'},
    {at: 7.7, text: 'Any provider. One flag.', accent: 'provider'},
    {at: 11.0, text: 'Or mix them per role.', accent: 'mix'},
    {at: 12.75, text: 'Or build from one sentence.', accent: 'one sentence'},
    {at: 15.0, text: 'Each ingot runs its proof.', accent: 'proof'},
    {at: 17.6, text: 'A fresh warden judges it.', accent: 'warden'},
    {at: 20.3, text: ''},
    {at: 22.75, text: 'Point it at an issue.', accent: 'issue'},
  ],
  scenes: [
    {
      id: 'fix',
      from: 0,
      to: 7.6,
      exit: {at: 7.35, keep: [0]},
      focus: [
        {at: 1.35, row: 1},
        {at: 1.8, row: 2},
        {at: 3.25, row: 3},
        {at: 4.3, row: 2},
        {at: 4.85, row: 4},
        {at: 5.55, row: 5},
        {at: 6.05, row: 6},
      ],
      lines: [
        {row: 0, at: 0, instant: true, segs: FIX_CMD, type: {at: 0.35, cps: 26, keep: 2}, cursor: {from: 0, to: 1.25}},
        {
          row: 1,
          at: 1.35,
          segs: [t('  issue ', 'cold'), t('#482', 'bright'), t(' export dates off by one', 'fg')],
        },
        {
          row: 2,
          at: 1.8,
          segs: [t('  '), g('spin', 'hot'), t(' reproducing', 'fg')],
          then: [
            {at: 2.75, segs: [t('  '), g('fail', 'warm'), t(' test_export_dates ', 'fg'), t('fails', 'warm')]},
            {at: 4.35, burst: true, segs: [t('  '), g('ok', 'pure'), t(' test_export_dates ', 'fg'), t('passes', 'pure')]},
          ],
        },
        {
          row: 3,
          at: 3.25,
          segs: [t('  '), g('spin', 'hot'), t(' patching ', 'fg'), t('src/export.py', 'bright')],
          then: [{at: 4.05, segs: [t('  '), t('±', 'hot', true), t(' patched  ', 'fg'), t('src/export.py', 'bright')]}],
        },
        {
          row: 4,
          at: 4.85,
          burst: true,
          segs: [t('  '), g('ok', 'pure'), t(' full suite        ', 'fg'), t('passes', 'pure')],
        },
        {
          row: 5,
          at: 5.55,
          segs: [t('  '), g('arrow', 'pure'), t(' branch ', 'cold'), t('slag/fix-482-export-dates', 'bright')],
        },
        {row: 6, at: 6.05, segs: [t('  '), g('arrow', 'pure'), t(' draft PR', 'pure', true), t(' opened', 'fg')]},
      ],
    },
    {
      id: 'provider',
      from: 7.6,
      to: 12.7,
      exit: {at: 4.85},
      lines: [
        {
          row: 0,
          at: 0,
          instant: true,
          segs: [...FIX_CMD, t(' --provider ', 'fg'), {roll: ROLL, every: ROLL_EVERY, from: ROLL_FROM, tone: 'hot'}],
          type: {at: 0.3, cps: 30, keep: 19},
          cursor: {from: 0.3, to: MIX_AT},
          dimAt: MIX_AT,
        },
        gridRow(2, PROVIDERS.slice(0, 3), 0.95),
        gridRow(3, PROVIDERS.slice(3, 6), 1.03),
        gridRow(4, PROVIDERS.slice(6, 9), 1.11),
        {row: 5, at: MIX_AT + 0.1, segs: [t('  plan ', 'cold'), g('arrow', 'cold'), t(' anthropic', 'hot')]},
        {row: 6, at: MIX_AT + 0.3, segs: [t('  work ', 'cold'), g('arrow', 'cold'), t(' cerebras', 'hot')]},
      ],
    },
    {
      id: 'forge',
      from: 12.7,
      to: 20.6,
      exit: {at: 7.6},
      focus: [
        {at: 1.95, row: 1},
        {at: 3.5, row: 3},
        {at: 5.0, row: 6},
      ],
      lines: [
        {
          row: 0,
          at: 0.15,
          segs: [PROMPT, t('slag', 'bright'), t(' "build a todo api" ', 'pure'), t('--temper 1', 'fg')],
          type: {at: 0.3, cps: 36, keep: 2},
          cursor: {from: 0.15, to: 1.4},
        },
        ...INGOTS.map(([id, work], i): Line => {
          const states: LineState[] = [];
          const add = (at: number, segs: Seg[], burst?: boolean) => states.push({at, segs, burst});
          if (id === 'i1') {
            add(1.95, molten(id, work));
            add(FORGED_AT[0], forged(id, work), true);
          } else if (id === 'i2') {
            add(2.65, molten(id, work));
            add(FORGED_AT[1], forged(id, work), true);
          } else if (id === 'i3') {
            add(2.65, molten(id, work));
            add(3.5, ingot(id, work, g('fail', 'warm'), 'proof fails', 'warm'));
            add(3.95, ingot(id, work, g('molten', 'hot'), 'heat 2', 'hot'));
            add(FORGED_AT[2], forged(id, work), true);
          } else {
            add(3.45, molten(id, work));
            add(FORGED_AT[3], forged(id, work), true);
          }
          return {row: i + 1, at: 1.5 + i * 0.08, segs: ore(id, work), then: states};
        }),
        {row: 5, at: 1.85, segs: [t('  assay ', 'cold'), {bar: {total: 4, forgedAt: FORGED_AT}}]},
        {
          row: 6,
          at: 5.0,
          segs: [t('  '), g('spin', 'hot'), t(' warden ', 'pure'), t('judging the result', 'fg')],
          then: [{at: 6.1, burst: true, segs: [t('  '), g('ok', 'pure'), t(' warden ', 'pure'), t('meets the bar', 'pure')]}],
        },
      ],
    },
    {
      id: 'rest',
      from: 22.6,
      to: 24,
      lines: [{row: 0, at: 0, instant: true, segs: [PROMPT], cursor: {from: 0, to: 99}}],
    },
  ],
  endCard: {from: 20.3, to: 23.0, tagline: 'Fix issues fast, with any model.', url: 'slag.dev'},
};
