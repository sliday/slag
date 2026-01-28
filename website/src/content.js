export const exampleIngots = [
  {
    id: "i1",
    status: "ore",
    solo: true,
    grade: 1,
    skill: "default",
    heat: 0,
    max: 5,
    proof: "test -f package.json",
    work: "Initialize project with package.json and git repo"
  },
  {
    id: "i2",
    status: "molten",
    solo: true,
    grade: 2,
    skill: "web",
    heat: 2,
    max: 5,
    proof: "test -f index.html && grep -q 'viewport' index.html",
    work: "Create mobile-first HTML structure with semantic markup"
  },
  {
    id: "i3",
    status: "forged",
    solo: true,
    grade: 1,
    skill: "cli",
    heat: 1,
    max: 5,
    proof: "test -f wrangler.toml && grep -q 'pages_build_output_dir' wrangler.toml",
    work: "Configure Cloudflare Pages deployment"
  },
  {
    id: "i4",
    status: "forged",
    solo: false,
    grade: 3,
    skill: "web",
    heat: 3,
    max: 8,
    proof: "npm run build && test -d dist",
    work: "Implement terminal-UI CSS theme with responsive breakpoints"
  },
  {
    id: "i5",
    status: "cracked",
    solo: false,
    grade: 4,
    skill: "web",
    heat: 8,
    max: 8,
    proof: "npx playwright test e2e/",
    work: "Add end-to-end browser tests with Playwright"
  },
  {
    id: "i6",
    status: "ore",
    solo: false,
    grade: 2,
    skill: "cli",
    heat: 0,
    max: 5,
    proof: "wrangler pages deploy dist --project-name=slag-dev --dry-run 2>&1 | grep -q 'Success'",
    work: "Deploy built site to Cloudflare Pages"
  }
];

export function renderIngots(ingots) {
  const container = document.getElementById('ingot-display');
  if (!container) return;

  container.innerHTML = ingots.map(ingot => `
    <div class="ingot-card" box="square">
      <div class="ingot-header">
        <span class="ingot-id">${ingot.id}</span>
        <span is="badge" class="${ingot.status}">${ingot.status}</span>
      </div>
      <div class="ingot-work">${ingot.work}</div>
      <div class="ingot-meta">
        <span>solo: ${ingot.solo ? 't' : 'nil'}</span>
        <span>grade: ${ingot.grade}</span>
        <span>skill: ${ingot.skill}</span>
        <span>heat: ${ingot.heat}/${ingot.max}</span>
      </div>
      <details class="ingot-proof">
        <summary>:proof</summary>
        <pre><code>${ingot.proof}</code></pre>
      </details>
    </div>
  `).join('');
}

export function toSExpression(ingot) {
  return `(ingot :id "${ingot.id}" :status ${ingot.status} :solo ${ingot.solo ? 't' : 'nil'} :grade ${ingot.grade} :skill ${ingot.skill} :heat ${ingot.heat} :max ${ingot.max} :proof "${ingot.proof}" :work "${ingot.work}")`;
}

export function logSExpressions(ingots) {
  console.log('slag · S-Expression Format:');
  console.log('');
  ingots.forEach(ingot => {
    console.log(toSExpression(ingot));
  });
}
