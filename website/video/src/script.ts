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

const COLS = 47;
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

// A report row: the label in the cold tone, padded to 11.
const row = (label: string, value: Seg[]): Seg[] => [t(`  ${label.padEnd(11)} `, 'cold'), ...value];

const PROMPT = t('$ ', 'cold');

// slag fix "Leaderboard order is wrong" in bench/issues/js-leaderboard/repo,
// SLAG_PROVIDER=cerebras, slag 2.9.0: FIXED in 1 attempt.
const FIX_CMD: Seg[] = [PROMPT, t('slag', 'bright'), t(' fix ', 'pure'), t('"Leaderboard order is wrong"', 'pure')];

const FIX_REPORT: Seg[][] = [
  rule('FIX', 'FIXED'),
  row('issue', [t('Leaderboard order is wrong')]),
  row('verdict', [t('FIXED', 'pure', true), t(' in 1 attempt')]),
  row('verified', [t('`npm test` exit 0 · 0.2s')]),
  row('branch', [t('slag/fix-leaderboard-… @ f63c37d')]),
  row('time', [t('wall 5s · api 4s · tools 0s')]),
];
const REVIEW: Seg[] = [t('  review: '), t('git show --stat HEAD', 'hot')];

// The poster, the first frame and the last: the command and its report,
// with the progress rows between them cut.
const RESULT_LINES: Line[] = [FIX_CMD, ...FIX_REPORT, REVIEW].map((segs, i) => ({row: i, at: 0, instant: true, segs}));
const RESULT_FOCUS = [{at: -1, row: 3}];

// The run itself, scene-relative. Rows past the eighth scroll the window.
const FIX_PROGRESS: [number, Seg[], boolean?][] = [
  [0.2, rule('FIX')],
  [0.4, status(t('░', 'cold'), [t('issue: Leaderboard order is wrong')])],
  [0.7, status(g('ok', 'cold'), [t('tests: `npm test` … exit 0 before any edit')])],
  [1.05, status(g('hammer', 'hot'), [t('attempt 1/3 · cerebras::qwen-3.8-27b')])],
  [1.4, tool('read', 'src/leaderboard.js', '15 lines')],
  [1.7, tool('write', 'tests/leaderboard.test.js')],
  [2.0, tool('edit', 'src/leaderboard.js')],
  [2.3, tool('run', 'node --test tests/*.…', '16 lines')],
  [2.7, status(g('square', 'pure'), [t('Root cause: src/leaderboard.js:8 called…')])],
  [3.25, status(g('ok', 'cold'), [t('verify: `npm test` exit 0 in 0.2s')]), true],
  [3.8, status(g('ok', 'cold'), [t('reproduced: without the fix … exit 1')]), true],
  [4.3, []],
  ...FIX_REPORT.map((segs, i): [number, Seg[], boolean?] => [4.35 + i * 0.2, segs, i === 2]),
  [5.55, []],
  [5.6, REVIEW],
];
const FIX_START = 1.0;
const FIX_LINES: Line[] = [
  {row: 0, at: 0, instant: true, segs: FIX_CMD},
  ...FIX_PROGRESS.map(([at, segs, burst], i): Line => ({row: i + 1, at, segs, burst})),
];
const FIX_FOCUS = [
  {at: 0.7, row: 3},
  {at: 1.05, row: 4},
  {at: 1.7, row: 6},
  {at: 3.25, row: 10},
  {at: 3.8, row: 11},
  {at: 4.75, row: 15},
];

// slag key --provider NAME for each preset, real output. The roll swaps
// every row at once, as a re-run prints them. The duel row is left out.
const PRESETS: [string, string, string][] = [
  ['groq', 'present (GROQ_API_KEY)', 'groq::openai/gpt-oss-120b'],
  ['cerebras', 'present (CEREBRAS_API_KEY)', 'cerebras::qwen-3.8-27b'],
  ['openai', 'present (OPENAI_API_KEY)', 'openai::gpt-4.1-mini'],
  ['anthropic', 'present (ANTHROPIC_API_KEY)', 'anthropic::claude-sonnet-4-5'],
  ['gemini', 'present (GEMINI_API_KEY)', 'gemini::gemini-flash-latest'],
  ['mistral', 'present (MISTRAL_API_KEY)', 'mistral::mistral-small-latest'],
  ['ollama', 'optional, none set', 'ollama::qwen3:8b'],
];
const ROLL_FROM = 1.7;
const ROLL_EVERY = 0.55;
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

// slag "a word-count CLI in Python" --temper 1 in an empty git repo,
// SLAG_PROVIDER=cerebras, slag 2.9.0. Lines from `slag replay <tape>
// --size 48x200`, so slag itself clipped them to 47 columns. Tool rows and
// heats 2 to 4 are left out. The run hit a 429 and never reached the
// warden, so no verdict is shown.
const FORGE_CMD: Seg[] = [PROMPT, t('slag', 'bright'), t(' "a word-count CLI in Python" ', 'pure'), t('--temper 1', 'fg')];
const lane = (id: string, par: boolean, grade: string, work: string): Seg[] => [
  t('    '),
  t(id, 'pure'),
  t('  '),
  t(par ? 'par' : 'seq', par ? 'hot' : 'cold'),
  t(` ${grade}`, 'cold'),
  t(`  ${work}`),
];
const anvil = (id: string, work: string, tags: string): Seg[] =>
  spread([t('  '), g('ingot', 'hot'), t(' '), t(id, 'pure'), t(`  ${work}`)], tags);
const proof = (cmd: string): Seg[] => [t('    proof  ', 'cold'), t(cmd, 'cold')];
const heat = (head: string, why: string): Seg[] => [t('    '), g('fail', 'warm'), t(` ${head}`, 'warm'), t(`  ${why}`)];
const done = (id: string, cmd: string, meta: string): Seg[] =>
  spread([t('  '), g('ok', 'pure'), t(` ${id} forged`, 'pure'), t(`  ${cmd}`, 'cold')], meta);
const FORGE_ROWS: Seg[][] = [
  rule('FOUNDER', 'casting mold', '0:04'),
  [t('  '), g('diamond', 'pure'), t(' mold', 'cold'), t('  3 ingots (3 simple, 0 complex, 0 web)')],
  lane('i1', true, 'g2', 'Create wordcount.py at the r…'),
  lane('i2', false, 'g2', 'Create tests/test_wordcount.…'),
  lane('i3', true, 'g1', 'Create README.md at the repo…'),
  [],
  rule('FORGE', undefined, '0:39'),
  [t('  '), g('hammer', 'hot'), t(' 2 anvils', 'hot'), t('  i1 · i3', 'pure')],
  anvil('i1', 'Create wordcount.py at the r…', 'g2 · cli'),
  proof('python -c "import wordcount as w;…'),
  anvil('i3', 'Create README.md at the repo root:…', 'g1'),
  proof("test -f README.md && grep -qi 'usa…"),
  heat('i1 heat 1/5', "the smith's check passed; t…"),
  done('i3', 'test -f READ…', 'heat 1/5 · 3:46'),
  [t('  '), g('square', 'pure'), t(' re-smelt rewrote i1')],
  heat('i1 heat 5/5', 'python -c'),
  [t('  '), g('resmelt', 'hot'), t(' i1 re-smelted', 'hot'), t('  Create wordcount.py at the…', 'cold')],
  done('i1', 'mkdir -p .wc…', 'heat 1/5 · 0:13'),
  done('i2', 'python -m un…', 'heat 1/5 · 0:19'),
  [],
  rule('ASSAY', undefined, '8:37'),
  [t('  █ ', 'pure'), t('FORGED', 'pure', true), t('   3/3 ingots · 4 retries · 8:37', 'cold')],
  // tui::bar at 47 columns: 37 cells, warm, hot, then bright by thirds.
  [t('  '), ...Array.from({length: 37}, (_, i): Seg => t('█', (['warm', 'hot', 'bright'] as const)[Math.floor((i * 3) / 37)])), t('  100%', 'pure')],
];
const FORGE_TIMES = [1.35, 1.55, 1.7, 1.8, 1.9, 2.1, 2.15, 2.4, 2.6, 2.7, 2.85, 2.95, 3.4, 3.8, 4.3, 4.45, 4.75, 5.15, 5.45, 5.75, 5.8, 6.0, 6.2];
const FORGE_LINES: Line[] = [
  {row: 0, at: 0.15, segs: FORGE_CMD, type: {at: 0.3, cps: 44, keep: 2}, cursor: {from: 0.15, to: 1.3}},
  ...FORGE_ROWS.map((segs, i): Line => ({row: i + 1, at: FORGE_TIMES[i], segs, burst: [13, 17, 18, 21].includes(i)})),
];
const FORGE_FOCUS = [
  {at: 1.55, row: 2},
  {at: 3.4, row: 13},
  {at: 4.75, row: 17},
  {at: 6.0, row: 22},
];

// Scene boundaries, absolute seconds.
const PROVIDER_START = 8.0;
const FORGE_START = 17.0;
const END_START = 25.2;
const AGAIN_START = 27.5;
const DURATION = 28.8;

export const heroScript: HeroScript = {
  duration: DURATION,
  captions: [
    {at: 0, text: 'Fixed, and proven by your tests.', accent: 'proven'},
    {at: 0.75, text: 'Point it at an issue.', accent: 'issue'},
    {at: 1.9, text: 'It runs your tests first.', accent: 'tests'},
    {at: 2.9, text: 'Then writes a test and the fix.', accent: 'a test'},
    {at: 4.2, text: 'It proves the fix both ways.', accent: 'both ways'},
    {at: 5.6, text: 'You review one commit.', accent: 'one commit'},
    {at: PROVIDER_START - 0.2, text: 'Any provider. One flag.', accent: 'provider'},
    {at: PROVIDER_START + MIX_AT, text: 'Or mix providers per role.', accent: 'per role'},
    {at: FORGE_START + 0.1, text: 'Or build from one sentence.', accent: 'one sentence'},
    {at: FORGE_START + 2.5, text: 'Each ingot must pass its proof.', accent: 'proof'},
    {at: FORGE_START + 4.4, text: 'A cracked ingot gets re-smelted.', accent: 're-smelted'},
    {at: FORGE_START + 5.85, text: 'Three of three ingots forged.', accent: 'forged'},
    {at: END_START, text: ''},
    {at: AGAIN_START + 0.15, text: 'Fixed, and proven by your tests.', accent: 'proven'},
  ],
  scenes: [
    {id: 'result', from: 0, to: FIX_START, exit: {at: 0.7, keep: [0]}, focus: RESULT_FOCUS, lines: RESULT_LINES},
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
      focus: [{at: 1.6, row: 5}],
      lines: [
        {
          row: 0,
          at: 0,
          segs: [PROMPT, t('export ', 'bright'), t('SLAG_MODEL_PLAN=openai::gpt-4.1-mini', 'pure')],
          type: {at: 0.1, cps: 48, keep: 2},
          cursor: {from: 0, to: 1.05},
        },
        {
          row: 1,
          at: 1.05,
          segs: [PROMPT, t('slag', 'bright'), t(' key ', 'pure'), t('--provider ', 'fg'), t('cerebras', 'hot')],
          type: {at: 1.1, cps: 48, keep: 2},
          cursor: {from: 1.05, to: 1.6},
        },
        ...MIX_PANEL.map((segs, i): Line => ({row: i + 2, at: 1.65 + i * 0.04, segs})),
      ],
    },
    {id: 'forge', from: FORGE_START, to: END_START + 0.3, exit: {at: END_START - FORGE_START}, focus: FORGE_FOCUS, lines: FORGE_LINES},
    {id: 'again', from: AGAIN_START, to: DURATION, focus: RESULT_FOCUS, lines: RESULT_LINES},
  ],
  endCard: {from: END_START, to: AGAIN_START + 0.4, tagline: 'Fix issues fast, with any model.', url: 'slag.dev'},
};
