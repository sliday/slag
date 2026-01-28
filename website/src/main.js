import '@fontsource/cascadia-code'
import './main.css'
import { exampleIngots, renderIngots, logSExpressions } from './content.js'

function initCopyButtons() {
  document.querySelectorAll('[data-copy]').forEach(el => {
    const wrapper = el.closest('.cmd-line') || el;
    const btn = document.createElement('button');
    btn.className = 'copy-btn';
    btn.textContent = 'copy';
    btn.addEventListener('click', () => {
      navigator.clipboard.writeText(el.textContent).then(() => {
        btn.textContent = 'copied';
        setTimeout(() => { btn.textContent = 'copy'; }, 1500);
      });
    });
    wrapper.appendChild(btn);
  });
}

function init() {
  renderIngots(exampleIngots);
  logSExpressions(exampleIngots);
  initCopyButtons();
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', init);
} else {
  init();
}
