import '@fontsource/cascadia-code'
import './main.css'
import { exampleIngots, renderIngots, logSExpressions } from './content.js'

function initCopyButtons() {
  document.querySelectorAll('[data-copy]').forEach(el => {
    const wrapper = el.closest('.cmd-line') || el;
    const btn = document.createElement('button');
    btn.className = 'copy-btn';
    btn.innerHTML = '📋';
    btn.title = 'Copy to clipboard';
    const copyText = () => {
      navigator.clipboard.writeText(el.textContent).then(() => {
        btn.innerHTML = '✓';
        setTimeout(() => { btn.innerHTML = '📋'; }, 1500);
      });
    };
    btn.addEventListener('click', copyText);
    // Make whole wrapper clickable
    wrapper.style.cursor = 'pointer';
    wrapper.addEventListener('click', (e) => {
      if (e.target !== btn) copyText();
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
