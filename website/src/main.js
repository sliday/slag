import '@fontsource/cascadia-code'
import './main.css'
import { exampleIngots, renderIngots, logSExpressions } from './content.js'

function initCopyButtons() {
  document.querySelectorAll('[data-copy]').forEach(el => {
    const wrapper = el.closest('.cmd-line') || el;
    const btn = document.createElement('button');
    btn.className = 'copy-btn';
    btn.textContent = 'Copy';

    const doCopy = (e) => {
      e.stopPropagation();
      navigator.clipboard.writeText(el.textContent).then(() => {
        btn.textContent = 'Copied';
        setTimeout(() => { btn.textContent = 'Copy'; }, 1500);
      });
    };

    btn.addEventListener('click', doCopy);
    wrapper.style.cursor = 'pointer';
    wrapper.addEventListener('click', doCopy);
    wrapper.appendChild(btn);
  });
}

function initCopyMarkdown() {
  const btn = document.getElementById('copy-md-btn');
  if (!btn) return;

  btn.addEventListener('click', async () => {
    try {
      const response = await fetch('/slag.md');
      const markdown = await response.text();
      await navigator.clipboard.writeText(markdown);
      btn.textContent = 'Copied!';
      btn.classList.add('copied');
      setTimeout(() => {
        btn.textContent = 'Copy as Markdown';
        btn.classList.remove('copied');
      }, 2000);
    } catch (err) {
      btn.textContent = 'Error';
      setTimeout(() => { btn.textContent = 'Copy as Markdown'; }, 2000);
    }
  });
}

function init() {
  renderIngots(exampleIngots);
  logSExpressions(exampleIngots);
  initCopyButtons();
  initCopyMarkdown();
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', init);
} else {
  init();
}
