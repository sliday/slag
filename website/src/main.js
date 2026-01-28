import '@fontsource/cascadia-code'
import './main.css'
import { exampleIngots, renderIngots, logSExpressions } from './content.js'

function initCopyButtons() {
  document.querySelectorAll('[data-copy]').forEach(el => {
    const wrapper = el.closest('.cmd-line') || el;
    const btn = document.createElement('button');
    btn.className = 'copy-btn';
    btn.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1"/></svg>';
    btn.title = 'Copy to clipboard';

    const doCopy = (e) => {
      e.stopPropagation();
      navigator.clipboard.writeText(el.textContent).then(() => {
        btn.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M20 6L9 17l-5-5"/></svg>';
        setTimeout(() => { btn.innerHTML = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1"/></svg>'; }, 1500);
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
