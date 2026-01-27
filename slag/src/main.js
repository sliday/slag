import './main.css'
import { exampleIngots, renderIngots, logSExpressions } from './content.js'

// Initialize the slag terminal UI
function init() {
  // Render example ingots
  renderIngots(exampleIngots);

  // Log s-expressions to console for reference
  logSExpressions(exampleIngots);

  // Add interactivity for status filtering (future enhancement)
  console.log('slag bash orchestrator initialized');
  console.log('View s-expression format in console above');
}

// Run on DOM ready
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', init);
} else {
  init();
}
