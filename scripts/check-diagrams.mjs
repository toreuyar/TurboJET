import { readFileSync } from 'fs';
import { join } from 'path';
import { homedir } from 'os';
import { pathToFileURL } from 'url';

const deps = process.env.DIAGRAM_CHECKER_DEPS || join(homedir(), '.local/share/agentic-console/diagram-checker');

async function loadJSDOM() {
  const jsdomPkg = JSON.parse(readFileSync(join(deps, 'node_modules/jsdom/package.json'), 'utf8'));
  const jsdomUrl = pathToFileURL(join(deps, 'node_modules/jsdom', jsdomPkg.main)).href;
  const jsdom = await import(jsdomUrl);
  return jsdom.JSDOM || jsdom.default.JSDOM;
}

async function loadMermaid() {
  const mermaidUrl = pathToFileURL(join(deps, 'node_modules/mermaid/dist/mermaid.esm.min.mjs')).href;
  return (await import(mermaidUrl)).default;
}

const file = process.argv[2];
if (!file) { console.error('usage: node scripts/check-diagrams.mjs <html-file>'); process.exit(2); }

const html = readFileSync(file, 'utf8');
const blocks = [...html.matchAll(/<div class="mermaid">\s*([\s\S]*?)\s*<\/div>/g)].map(m => m[1]);
if (!blocks.length) { console.error('no mermaid blocks found in ' + file); process.exit(1); }

let JSDOM, mermaid;
try {
  JSDOM = await loadJSDOM();
} catch (e) {
  console.error('diagram checker dependencies missing at: ' + deps);
  console.error('install with: npm -p ' + deps + ' i mermaid jsdom');
  process.exit(2);
}

const dom = new JSDOM('<!DOCTYPE html><body></body>');
globalThis.window = dom.window;
globalThis.document = dom.window.document;

mermaid = await loadMermaid();
mermaid.initialize({ startOnLoad: false });

let failed = 0;
for (let i = 0; i < blocks.length; i++) {
  try {
    await mermaid.parse(blocks[i]);
    console.log(`ok    diagram ${i + 1}`);
  } catch (e) {
    failed++;
    const msg = (e && e.message) ? e.message.split('\n').slice(0, 3).join(' | ') : String(e);
    console.error(`FAIL  diagram ${i + 1}: ${msg}`);
  }
}
console.log(`${blocks.length - failed}/${blocks.length} diagrams valid`);
process.exit(failed ? 1 : 0);
