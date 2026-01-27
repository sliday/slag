// Example ingot data in s-expression format
// (ingot :id "i1" :status ore :solo t :grade 1 :heat 0 :max 5 :proof "test -d project" :work "Create project directory")
// (ingot :id "i2" :status molten :solo t :grade 2 :heat 1 :max 5 :proof "test -f package.json" :work "Initialize vite project")
export const exampleIngots = [
  {
    id: "i1",
    status: "ore",
    solo: true,
    grade: 1,
    heat: 0,
    max: 5,
    proof: "test -d project",
    work: "Create project directory"
  },
  {
    id: "i2",
    status: "molten",
    solo: true,
    grade: 2,
    heat: 1,
    max: 5,
    proof: "test -f package.json && grep -q vite package.json",
    work: "Initialize vite project"
  },
  {
    id: "i3",
    status: "forged",
    solo: false,
    grade: 1,
    heat: 2,
    max: 5,
    proof: "npm run build",
    work: "Configure build pipeline"
  },
  {
    id: "i4",
    status: "ore",
    solo: false,
    grade: 3,
    heat: 0,
    max: 5,
    proof: "npm test && npm run lint",
    work: "Add comprehensive test suite"
  }
];

// Render ingots to the DOM
export function renderIngots(ingots) {
  const container = document.getElementById('ingot-display');
  if (!container) return;

  container.innerHTML = ingots.map(ingot => `
    <div class="ingot-item">
      <div class="ingot-header">
        <span class="ingot-id">${ingot.id}</span>
        <span class="ingot-status ${ingot.status}">${ingot.status}</span>
      </div>
      <div class="ingot-work">${ingot.work}</div>
      <div class="ingot-meta">
        <span>solo: ${ingot.solo ? 't' : 'nil'}</span>
        <span>grade: ${ingot.grade}</span>
        <span>heat: ${ingot.heat}/${ingot.max}</span>
      </div>
      <details style="margin-top: 0.5rem;">
        <summary style="color: var(--text-secondary); cursor: pointer; font-size: 0.75rem;">proof command</summary>
        <pre style="margin-top: 0.5rem; padding: 0.5rem; background: var(--bg-primary); border-radius: 3px; overflow-x: auto;"><code>${ingot.proof}</code></pre>
      </details>
    </div>
  `).join('');
}

// Generate s-expression format from ingot data
export function toSExpression(ingot) {
  return `(ingot :id "${ingot.id}" :status ${ingot.status} :solo ${ingot.solo ? 't' : 'nil'} :grade ${ingot.grade} :heat ${ingot.heat} :max ${ingot.max} :proof "${ingot.proof}" :work "${ingot.work}")`;
}

// Display s-expressions in the console
export function logSExpressions(ingots) {
  console.log('S-Expression Format:');
  console.log('');
  ingots.forEach(ingot => {
    console.log(toSExpression(ingot));
  });
}
