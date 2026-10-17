import type {Tone} from './theme';

// The whole video is this file. Captions use absolute seconds. Terminal
// lines use seconds from the start of their scene. When the real CLI
// output changes, edit the strings here and run ./render.sh.
//
// Every terminal line below is copied from a real run of slag 2.9.0, not
// written for the video. The window is 47 columns wide, so long lines are
// shortened with an ellipsis, and a few rows of each run are left out.
// Sources: refs/real/ in the gauntlet notes, `slag key --provider NAME`,
// and the forge transcript named at FORGE_LINES.

export type Glyph = 'ok' | 'fail' | 'spin' | 'arrow' | 'ore' | 'molten' | 'forged' | 'hammer' | 'square' | 'ingot' | 'diamond' | 'resmelt';

export type Lit = {from: number; to: number; tone: Tone};

export type TextSeg = {text: string; tone?: Tone; bold?: boolean; lit?: Lit[]};
export type GlyphSeg = {glyph: Glyph; tone?: Tone};
// A word that rolls through `words`, one every `every` seconds from `from`.
// `slide` animates the change like an edited command; output rows swap at
// once, the way a re-run prints them.
export type RollSeg = {roll: string[]; every: number; from: number; tone?: Tone; slide?: boolean};
export type Seg = TextSeg | GlyphSeg | RollSeg;

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

const COLS = 49;
const len = (segs: Seg[]) => segs.reduce((n, s) => n + ('text' in s ? s.text.length : 1), 0);

// `── NAME · detail ────── right`, as tui::rule_line draws it.
const rule = (name: string, detail?: string, right?: string): Seg[] => {
  const head: Seg[] = [t('── ', 'cold'), t(name, 'pure', true)];
  if (detail) head.push(t(` · ${detail}`, 'cold'));
  const tail = right ? ` ${right}` : '';
  const fill = COLS - len(head) - 1 - tail.length;
  return [...head, t(` ${'─'.repeat(Math.max(1, fill))}${tail}`, 'cold')];
};

// Left text, then `right` flush to the last column, as tui::spread does.
const spread = (left: Seg[], right: string, tone: Tone = 'cold'): Seg[] => [
  ...left,
  t(' '.repeat(Math.max(2, COLS - len(left) - right.length))),
  t(right, tone),
];

// A tool row: `  ✓ read   src/leaderboard.js       15 lines`.
const tool = (verb: string, subject: string, meta?: string): Seg[] => {
  const left: Seg[] = [t('  '), g('ok', 'cold'), t(` ${verb.padEnd(6)} `, 'cold'), t(subject)];
  return meta ? spread(left, meta) : left;
};

// A status line: one colored glyph, then plain text.
const status = (glyph: Seg, text: Seg[]): Seg[] => [t('  '), glyph, t(' '), ...text];


const PROMPT = t('$ ', 'cold');

// engine::events::preview: keep max-1 characters, trim, add an ellipsis.
// slag clips every report value with it, so this is what it prints at 49
// columns.
const clip = (s: string, max: number) =>
  s.length <= max ? s : `${s.slice(0, max - 1).trimEnd()}…`;

// The fix report's 6-wide label column (pipeline/fix.rs report()).
const label = (name: string): Seg => t(`  ${name.padEnd(6)}  `, 'cold');

// slag fix "Leaderboard order is wrong" in bench/issues/js-leaderboard/repo,
// SLAG_PROVIDER=cerebras, slag 2.9.0 after the done-screen rework:
// FIXED in 1 attempt, wall 4s.
const FIX_CMD: Seg[] = [PROMPT, t('slag', 'bright'), t(' fix ', 'pure'), t('"Leaderboard order is wrong"', 'pure')];
const VERDICT: Seg[] = [t('  '), g('ok', 'pure'), t(' '), t('FIXED', 'pure', true), t(`  ${clip('Leaderboard order is wrong · 1 attempt', COLS - 11)}`)];
const NEXT: Seg[] = [label('next'), t('git show --stat HEAD', 'hot'), t(`  ${clip('review the commit', COLS - 12 - 20)}`, 'cold')];
const PROOF: Seg[] = spread([label('proof'), g('ok', 'pure'), t(' npm test')], 'exit 0 · 0.2s');
const WITHOUT: Seg[] = [label(''), g('fail', 'warm'), t(clip(' without the fix: exit 1, so the new test catches the bug', COLS - 11))];
const WORK: Seg[] = [label('work'), t(clip('slag/fix-leaderboard-order-is-wrong @ fa6e958', COLS - 10))];
const RUN: Seg[] = [label('run'), t('wall 4s · api 3s · tools 0s')];

// The poster, the first frame and the last: the command over its done
// screen. The progress rows, the blank row and the diff rows are cut.
const RESULT_LINES: Line[] = [FIX_CMD, rule('FIX', 'FIXED'), VERDICT, NEXT, PROOF, WITHOUT, WORK, RUN].map(
  (segs, i) => ({row: i, at: 0, instant: true, segs}),
);
const RESULT_FOCUS = [{at: -1, row: 2}];

// The run itself, scene-relative. Rows past the eighth scroll the window.
// The diff rows and the phase timings are cut from the report.
const FIX_PROGRESS: [number, Seg[], boolean?][] = [
  [0.2, rule('FIX')],
  [0.45, status(t('░', 'cold'), [t('issue: Leaderboard order is wrong')])],
  [0.8, status(g('ok', 'cold'), [t('tests: `npm test` … exit 0 before any edit')])],
  [1.25, status(g('hammer', 'hot'), [t('attempt 1/3 · cerebras::qwen-3.8-27b')])],
  [1.75, tool('read', 'src/leaderboard.js', '15 lines')],
  [2.05, tool('write', 'tests/leaderboard.test.js')],
  [2.4, tool('edit', 'src/leaderboard.js')],
  [2.75, tool('run', 'npm test', '17 lines')],
  [3.1, status(g('square', 'pure'), [t(clip('All 5 tests pass, including the two new regression tests.', COLS - 4))])],
  [3.6, status(g('ok', 'cold'), [t('verify: `npm test` exit 0 in 0.2s')]), true],
  [4.25, status(g('ok', 'cold'), [t('reproduced: without the fix … exit 1')]), true],
  [5.0, []],
  [5.05, rule('FIX', 'FIXED')],
  [5.25, VERDICT, true],
  [5.45, NEXT],
  [5.6, []],
  [5.65, PROOF],
  [5.85, WITHOUT],
  [6.05, WORK],
  [6.25, RUN],
];
const FIX_START = 1.9;
const FIX_LINES: Line[] = [
  {row: 0, at: 0, instant: true, segs: FIX_CMD},
  ...FIX_PROGRESS.map(([at, segs, burst], i): Line => ({row: i + 1, at, segs, burst})),
];
const FIX_FOCUS = [
  {at: 0.8, row: 3},
  {at: 2.05, row: 6},
  {at: 3.6, row: 10},
  {at: 4.25, row: 11},
  {at: 5.25, row: 14},
];

// slag key --provider NAME for each preset, real output. The roll swaps
// every row at once, as a re-run prints them. The duel row is left out.
const PRESETS: [string, string, string][] = [
  ['groq', 'present (GROQ_API_KEY)', 'groq::openai/gpt-oss-120b'],
  ['cerebras', 'present (CEREBRAS_API_KEY)', 'cerebras::qwen-3.8-27b'],
  ['anthropic', 'present (ANTHROPIC_API_KEY)', 'anthropic::claude-sonnet-4-5'],
  ['ollama', 'optional, none set', 'ollama::qwen3:8b'],
];
const ROLL_FROM = 1.7;
const ROLL_EVERY = 0.6;
const MIX_AT = ROLL_FROM + (PRESETS.length - 1) * ROLL_EVERY + 0.6;
const roll = (k: number, tone: Tone = 'fg', slide = false): Seg => ({
  roll: PRESETS.map((p) => (k === 0 ? p[0] : p[k])),
  every: ROLL_EVERY,
  from: ROLL_FROM,
  tone,
  slide,
});
const KEY_CMD: Seg[] = [PROMPT, t('slag', 'bright'), t(' key ', 'pure'), t('--provider ', 'fg'), roll(0, 'hot', true)];
const PANEL: Seg[][] = [
  [t('  provider: '), roll(0)],
  [t('  key: '), roll(1)],
  [t('  work: '), roll(2)],
  [t('  plan: '), roll(2)],
  [t('  alt: '), roll(2)],
  [t('  judge: '), roll(2)],
];
// export SLAG_MODEL_PLAN=openai::gpt-4.1-mini, then slag key --provider
// cerebras: the plan role moves to OpenAI, the rest stay on Cerebras.
const MIX_PANEL: Seg[][] = [
  [t('  provider: cerebras')],
  [t('  key: present (CEREBRAS_API_KEY)')],
  [t('  work: cerebras::qwen-3.8-27b')],
  [t('  plan: openai::gpt-4.1-mini')],
  [t('  alt: cerebras::qwen-3.8-27b')],
  [t('  judge: cerebras::qwen-3.8-27b')],
];

// slag "a tested Python greet() function" --temper 1 in an empty git repo,
// OpenRouter with openai/gpt-oss-120b on every role, slag 2.9.0 with the
// --temper fix. Lines from `slag replay <tape> --size 50x300`, so slag
// itself clipped them to 49 columns. The warden sent back the plan, the
// one ingot forged, and the warden then rejected the goal on a real gap.
// Tool rows, the survey, and the blueprint review are cut.
const sentBack = (doc: string, lines: string[]): Seg[][] => [
  [t('  '), t('!', 'bright'), t(` warden sent back the ${doc}`, 'bright'), t(`  ${lines[0]}`)],
  ...lines.slice(1).map((l): Seg[] => [t(`    ${l}`)]),
];
const FORGE_ROWS: Seg[][] = [
  [t('> ', 'cold'), t('--temper 1', 'fg')],
  rule('FOUNDER', 'casting mold', '1:02'),
  ...sentBack('plan', ['The plan does not', 'ensure that tests assert a TypeError for', 'non‑string input (and therefore does not gua…']),
  [t('  '), g('diamond', 'pure'), t(' mold', 'cold'), t('  1 ingots (0 simple, 1 complex, 0 web)')],
  [],
  rule('FORGE', undefined, '2:46'),
  spread([t('  '), g('ok', 'pure'), t(' i1 forged', 'pure'), t('  cd greet_pkg &…', 'cold')], 'heat 1/5 · 5:36'),
  [],
  rule('WARDEN', 'round 1', '8:23'),
  [t('  '), g('fail', 'warm'), t(' warden', 'warm'), t(' round 1', 'cold'), t('  goal not met — `pip install')],
  [t('    .` fails due to external‑managed environment')],
  [t('    restrictions (PEP 668)')],
  [],
  rule('ASSAY', undefined, '9:07'),
  [t('  '), g('fail', 'warm'), t(' '), t('GOAL NOT MET', 'warm', true), t('  every task passed its proof; t…')],
  [t('    `pip install .` fails due to external‑managed', 'warm')],
  [t('    environment restrictions (PEP 668)', 'warm')],
  [t('  next    ', 'cold'), t('slag --temper 2 resume', 'hot'), t('  forges the gap…', 'cold')],
];
const FORGE_TIMES = [1.3, 1.85, 2.1, 2.15, 2.2, 2.6, 2.9, 2.95, 3.4, 3.9, 3.95, 4.5, 4.55, 4.6, 5.4, 5.45, 5.7, 5.75, 5.8, 6.1];
const FORGE_CMD: Seg[] = [PROMPT, t('slag', 'bright'), t(' "a tested Python greet() function" ', 'pure'), t('\\', 'cold')];
const FORGE_LINES: Line[] = [
  {row: 0, at: 0.15, segs: FORGE_CMD, type: {at: 0.3, cps: 44, keep: 2}, cursor: {from: 0.15, to: 1.3}},
  ...FORGE_ROWS.map((segs, i): Line => ({
    row: i + 1,
    at: FORGE_TIMES[i],
    segs,
    burst: i === 8,
    ...(i === 0 ? {type: {at: 1.35, cps: 44, keep: 2}, cursor: {from: 1.3, to: 1.75}} : {}),
  })),
];
const FORGE_FOCUS = [
  {at: 2.1, row: 3},
  {at: 3.4, row: 9},
  {at: 4.5, row: 12},
  {at: 5.7, row: 17},
];

// Scene boundaries, absolute seconds.
const PROVIDER_START = FIX_START + 8.2;
const FORGE_START = PROVIDER_START + MIX_AT + 3.1;
const END_START = FORGE_START + 8.4;
const AGAIN_START = END_START + 2.3;
const DURATION = AGAIN_START + 1.2;

export const heroScript: HeroScript = {
  duration: DURATION,
  captions: [
    {at: 0, text: 'Fixed, and proven by your tests.', accent: 'proven'},
    {at: 1.65, text: 'Point it at an issue.', accent: 'issue'},
    {at: FIX_START + 1.6, text: 'It writes a test, then the fix.', accent: 'a test'},
    {at: FIX_START + 3.45, text: 'It proves the fix both ways.', accent: 'both ways'},
    {at: FIX_START + 5.15, text: 'You review one commit.', accent: 'one commit'},
    {at: PROVIDER_START - 0.2, text: 'Any provider. One flag.', accent: 'provider'},
    {at: PROVIDER_START + MIX_AT, text: 'Or mix providers per role.', accent: 'per role'},
    {at: FORGE_START, text: 'Or build from one sentence.', accent: 'one sentence'},
    {at: FORGE_START + 1.95, text: 'A fresh warden checks the plan.', accent: 'warden'},
    {at: FORGE_START + 3.9, text: 'Then it judges the result.', accent: 'judges'},
    {at: FORGE_START + 5.6, text: 'Proofs passed. The goal did not.', accent: 'goal'},
    {at: END_START, text: ''},
    {at: AGAIN_START + 0.15, text: 'Fixed, and proven by your tests.', accent: 'proven'},
  ],
  scenes: [
    {id: 'result', from: 0, to: FIX_START, exit: {at: 1.6, keep: [0]}, focus: RESULT_FOCUS, lines: RESULT_LINES},
    {
      id: 'fix',
      from: FIX_START,
      to: PROVIDER_START,
      exit: {at: PROVIDER_START - FIX_START - 0.3},
      focus: FIX_FOCUS,
      lines: FIX_LINES,
    },
    {
      id: 'provider',
      from: PROVIDER_START,
      to: PROVIDER_START + MIX_AT,
      exit: {at: MIX_AT - 0.25},
      focus: [{at: ROLL_FROM - 0.4, row: 1}],
      lines: [
        {row: 0, at: 0.1, segs: KEY_CMD, type: {at: 0.25, cps: 32, keep: 2}, cursor: {from: 0.1, to: 1.0}},
        ...PANEL.map((segs, i): Line => ({row: i + 1, at: 1.1 + i * 0.04, segs})),
      ],
    },
    {
      id: 'mix',
      from: PROVIDER_START + MIX_AT,
      to: FORGE_START,
      exit: {at: FORGE_START - PROVIDER_START - MIX_AT - 0.3},
      focus: [{at: 1.3, row: 5}],
      lines: [
        {
          row: 0,
          at: 0,
          segs: [PROMPT, t('export ', 'bright'), t('SLAG_MODEL_PLAN=openai::gpt-4.1-mini', 'pure')],
          type: {at: 0.1, cps: 60, keep: 2},
          cursor: {from: 0, to: 0.8},
        },
        {
          row: 1,
          at: 0.8,
          segs: [PROMPT, t('slag', 'bright'), t(' key ', 'pure'), t('--provider ', 'fg'), t('cerebras', 'hot')],
          type: {at: 0.85, cps: 60, keep: 2},
          cursor: {from: 0.8, to: 1.25},
        },
        ...MIX_PANEL.map((segs, i): Line => ({row: i + 2, at: 1.3 + i * 0.04, segs})),
      ],
    },
    {id: 'forge', from: FORGE_START, to: END_START + 0.3, exit: {at: END_START - FORGE_START}, focus: FORGE_FOCUS, lines: FORGE_LINES},
    {id: 'again', from: AGAIN_START, to: DURATION, focus: RESULT_FOCUS, lines: RESULT_LINES},
  ],
  endCard: {from: END_START, to: AGAIN_START + 0.4, tagline: 'Fix issues fast, with any model.', url: 'slag.dev'},
};
