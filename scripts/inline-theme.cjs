const fs = require('fs');
const path = require('path');

const root = path.join(__dirname, '..');
const file = path.join(root, 'ui', 'index.html');
const css = fs.readFileSync(path.join(root, 'ui', 'theme.css'), 'utf8').trim();
let html = fs.readFileSync(file, 'utf8');

if (!/<style>[\s\S]*?<\/style>/.test(html)) {
  console.error('no <style> block found');
  process.exit(1);
}
html = html.replace(/<style>[\s\S]*?<\/style>/, '<style>\n' + css + '\n</style>');
fs.writeFileSync(file, html);
console.log('theme inlined');
