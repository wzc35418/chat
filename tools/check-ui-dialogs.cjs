#!/usr/bin/env node
// 廉价但有效的 UI 对话框自检：
//   每个 AlertDialog / DialogWindow 必须有可用的关闭路径——
//   onDismissRequest 指向 handlers 回调（或显式闭包），且不能是空 lambda；
//   confirmButton / dismissButton 的 onClick 不能是空 lambda。
//
// 为什么要这个：R-307 重写界面时把“关闭”按钮和 onDismissRequest 写成了 `{ }`，
// 结果群组详情对话框**完全关不掉**（issue #1），而编译、冒烟、截屏全都不会报。
const fs = require('node:fs');
const path = require('node:path');

const defaultRoots = [
  path.join(__dirname, '..', 'shared', 'ui', 'src'),
  path.join(__dirname, '..', 'apps', 'desktop', 'src'),
];
// UI_DIALOG_ROOTS 允许指向临时目录（用于自检的负向测试：故意放一个坏对话框，检查器必须报错）。
const roots = process.env.UI_DIALOG_ROOTS
  ? process.env.UI_DIALOG_ROOTS.split(path.delimiter).filter(Boolean)
  : defaultRoots;

function walk(dir, out = []) {
  if (!fs.existsSync(dir)) return out;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) walk(full, out);
    else if (entry.name.endsWith('.kt')) out.push(full);
  }
  return out;
}

const EMPTY_LAMBDA = /^(?:\{\s*\}|\{\s*\(\)\s*->\s*\}|\{\s*_\s*->\s*\})$/;

/** 从 index 之后的第一个 '(' 开始，取到配对的 ')'（圆括号计数，跳过字符串里的括号）。 */
function argsAfter(text, index) {
  const open = text.indexOf('(', index);
  if (open < 0) return null;
  let depth = 0;
  let inString = false;
  for (let i = open; i < text.length; i++) {
    const ch = text[i];
    if (ch === '"' && text[i - 1] !== '\\') inString = !inString;
    if (inString) continue;
    if (ch === '(') depth++;
    else if (ch === ')') {
      depth--;
      if (depth === 0) return { start: open, end: i, text: text.slice(open, i + 1) };
    }
  }
  return null;
}

/** 从 index 之后第一个 '{' 开始做配平，返回整块。 */
function braceAfter(text, index) {
  const open = text.indexOf('{', index);
  if (open < 0) return null;
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    if (text[i] === '{') depth++;
    else if (text[i] === '}') {
      depth--;
      if (depth === 0) return { start: open, end: i, text: text.slice(open, i + 1) };
    }
  }
  return null;
}

const problems = [];
let dialogs = 0;
for (const root of roots) {
  for (const file of walk(root)) {
    const source = fs.readFileSync(file, 'utf8');
    const relative = path.relative(path.join(__dirname, '..'), file).replace(/\\/g, '/');
    const matcher = /AlertDialog\s*\(|DialogWindow\s*\(/g;
    let match;
    while ((match = matcher.exec(source)) !== null) {
      dialogs++;
      const line = source.slice(0, match.index).split('\n').length;
      const block = argsAfter(source, match.index);
      if (!block) { problems.push(relative + ':' + line + ' 对话框参数块无法解析'); continue; }
      const body = block.text;

      const dismiss = body.match(/onDismissRequest\s*=\s*(\{[^}]*\}|[A-Za-z_][\w.]*)/);
      if (!dismiss) {
        problems.push(relative + ':' + line + ' 缺少 onDismissRequest（对话框只能靠系统键关）');
      } else if (EMPTY_LAMBDA.test(dismiss[1].trim())) {
        problems.push(relative + ':' + line + ' onDismissRequest 是空 lambda（点外面关不掉）');
      }

      let sawButton = false;
      for (const name of ['confirmButton', 'dismissButton']) {
        const at = body.indexOf(name + ' =');
        if (at < 0) continue;
        sawButton = true;
        const block = braceAfter(body, at);
        const inner = block ? block.text : body.slice(at);
        const onClick = inner.match(/onClick\s*=\s*(\{[\s\S]*?\}|[A-Za-z_][\w.]*)/);
        if (!onClick) {
          problems.push(relative + ':' + line + ' ' + name + ' 里没有 onClick');
        } else if (EMPTY_LAMBDA.test(onClick[1].trim())) {
          problems.push(relative + ':' + line + ' ' + name + ' 的 onClick 是空 lambda（按钮点了没反应）');
        }
      }
      if (!sawButton) problems.push(relative + ':' + line + ' 没有任何按钮（无法关闭）');
    }
  }
}


// ── 第二遍：按钮/可点区域不允许“看着能点但什么都不做” ─────────────────────
const CLICKABLE = /(?:^|[^\w.])(Button|TextButton|OutlinedButton|FilledTonalButton|IconButton)\s*\(/g;
const EMPTY_BLOCK = /^(?:\{\s*\}|\{\s*\(\)\s*->\s*\}|\{\s*_\s*->\s*\})$/;
for (const root of roots) {
  for (const file of walk(root)) {
    const source = fs.readFileSync(file, 'utf8');
    const relative = path.relative(path.join(__dirname, '..'), file).replace(/\\/g, '/');
    let match;
    while ((match = CLICKABLE.exec(source)) !== null) {
      const line = source.slice(0, match.index).split('\n').length;
      const block = argsAfter(source, match.index);
      if (!block) continue;
      const onClick = block.text.match(/onClick\s*=\s*(\{[^}]*\}|[A-Za-z_][\w.]*)/);
      if (!onClick) {
        problems.push(relative + ':' + line + ' ' + match[1] + ' 没有 onClick（点了不会有事发生）');
      } else if (EMPTY_BLOCK.test(onClick[1].trim())) {
        // 例外：**显式禁用**的占位按钮（enabled = false）不算“点了没反应”——它本来就不可点。
        const disabled = /enabled\s*=\s*false/.test(block.text);
        if (!disabled) {
          problems.push(relative + ':' + line + ' ' + match[1] + ' 的 onClick 是空 lambda（按钮点了没反应）');
        }
      }
    }
    const clickableMatcher = /\.clickable\s*(?:\([^)]*\))?\s*(\{[^}]*\})/g;
    let clickableMatch;
    while ((clickableMatch = clickableMatcher.exec(source)) !== null) {
      if (EMPTY_BLOCK.test(clickableMatch[1].trim())) {
        const line = source.slice(0, clickableMatch.index).split('\n').length;
        problems.push(relative + ':' + line + ' clickable 是空 lambda（点了没反应）');
      }
    }
  }
}

if (problems.length === 0) {
  console.log('UI DIALOGS: PASS（' + dialogs + ' 个对话框都有可用关闭路径；按钮/clickable 无空 onClick）');
  process.exit(0);
}
console.log('UI DIALOGS: FAIL（' + problems.length + ' 处问题 / 共 ' + dialogs + ' 个对话框）');
for (const problem of problems) console.log('  - ' + problem);
process.exit(1);
