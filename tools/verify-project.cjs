// Requires Node.js 24. Structure checks only, not a product test.
(function verifyProject() {
  const fs=require('node:fs'), path=require('node:path');
  const root=path.resolve(__dirname,'..');
  const report={date:'2026-10-18',status:'PASS',scope:'delivery-structure-only',checks:[],limitations:['未运行 Rust/Kotlin 编译','未验证 MLS/密码学/跨端/网络/部署行为','SQL仅在内存普通SQLite中验证，不证明加密存储可用']};
  const fail=(message)=>{report.status='FAIL';report.checks.push({status:'FAIL',message});};
  const ok=(message)=>report.checks.push({status:'PASS',message});
  const walk=(dir)=>fs.readdirSync(dir,{withFileTypes:true}).flatMap(x=>x.isDirectory()?(['target','.gradle','.kotlin','.tools','build','.git','node_modules'].includes(x.name)?[]:walk(path.join(dir,x.name))):[path.join(dir,x.name)]);
  const files=walk(root); const rel=p=>path.relative(root,p).replaceAll('\\','/');
  const catalog=JSON.parse(fs.readFileSync(path.join(root,'docs/implementation/function-catalog.json'),'utf8'));
  const ids=new Set(), names=new Set();
  for(const f of catalog.functions) {
    if(ids.has(f.id)||names.has(f.name)) fail('重复函数 '+f.id);
    ids.add(f.id); names.add(f.name);
    for(const p of [f.file,f.plan]) if(!fs.existsSync(path.join(root,p))) fail('缺少函数文件 '+p);
    if(f.steps.length<3||!f.tests.length||!f.signature||!f.tx||!f.errors) fail('函数契约不完整 '+f.id);
    const plan=fs.readFileSync(path.join(root,f.plan),'utf8');
    if(!plan.includes('id="'+f.id.toLowerCase()+'"')) fail('函数锚点缺失 '+f.id);
  }
  for(const f of catalog.functions) for(const call of f.calls) if(!names.has(call)) fail('不存在的调用 '+call);
  if(catalog.count!==catalog.functions.length) fail('函数数目不一致');
  ok('函数目录与源码位置、唯一ID、契约字段、调用引用已检查：'+catalog.count);
  let links=0;
  for(const file of files.filter(x=>x.endsWith('.md'))) {
    const content=fs.readFileSync(file,'utf8');
    for(const match of content.matchAll(/\]\(([^)]+)\)/g)) {
      const raw=match[1].trim().replace(/^<|>$/g,'');
      if(/^[a-zA-Z][\w+.-]*:/.test(raw)) continue;
      const [p,anchor]=raw.split('#');
      let resolved; try { resolved=p?path.resolve(path.dirname(file),decodeURIComponent(p)):file; }catch{ fail('链接编码错误 '+rel(file)); continue; }
      if(!fs.existsSync(resolved)) {fail('失效链接 '+rel(file)+' -> '+raw);continue;}
      if(anchor && resolved.endsWith('.md')) {
        const target=fs.readFileSync(resolved,'utf8');
        const explicit='id="'+anchor+'"';
        if(!target.includes(explicit)) fail('失效锚点 '+rel(file)+' -> '+raw);
      }
      links++;
    }
  }
  ok('本地Markdown链接及显式函数锚点已检查：'+links);
  let jsonCount=0;
  for(const file of files.filter(x=>x.endsWith('.json'))) {try {JSON.parse(fs.readFileSync(file,'utf8')); jsonCount++;}catch(e){fail('JSON语法 '+rel(file)+': '+e.message);}}
  ok('JSON语法已检查：'+jsonCount);
  function validate(value,schema,loc) {
    if(schema.const!==undefined && value!==schema.const) fail(loc+' const');
    if(schema.enum && !schema.enum.includes(value)) fail(loc+' enum');
    const type=schema.type;
    if(type==='object') {
      if(!value||typeof value!=='object'||Array.isArray(value)) {fail(loc+' object'); return;}
      for(const key of schema.required||[]) if(!(key in value)) fail(loc+' 缺少 '+key);
      for(const [key,v] of Object.entries(value)) {if(schema.properties?.[key]) validate(v,schema.properties[key],loc+'.'+key); else if(schema.additionalProperties===false) fail(loc+' 未知字段 '+key);}
    } else if(type==='array') {
      if(!Array.isArray(value)) {fail(loc+' array'); return;}
      if(schema.minItems!==undefined&&value.length<schema.minItems) fail(loc+' minItems');
      if(schema.maxItems!==undefined&&value.length>schema.maxItems) fail(loc+' maxItems');
      if(schema.uniqueItems&&new Set(value.map(x=>JSON.stringify(x))).size!==value.length) fail(loc+' uniqueItems');
      value.forEach((x,i)=>validate(x,schema.items,loc+'['+i+']'));
    } else if(type==='string') {
      if(typeof value!=='string') {fail(loc+' string');return;}
      const len=Array.from(value).length;
      if(schema.minLength!==undefined&&len<schema.minLength) fail(loc+' minLength');
      if(schema.maxLength!==undefined&&len>schema.maxLength) fail(loc+' maxLength');
      if(schema.pattern&&!new RegExp(schema.pattern).test(value)) fail(loc+' pattern');
    } else if(type==='integer') {
      if(!Number.isSafeInteger(value)) fail(loc+' integer');
      if(schema.minimum!==undefined&&value<schema.minimum) fail(loc+' minimum');
      if(schema.maximum!==undefined&&value>schema.maximum) fail(loc+' maximum');
    }
  }
  const readJson=p=>JSON.parse(fs.readFileSync(path.join(root,p),'utf8'));
  const pluginSchema=readJson('schemas/plugin-manifest.schema.json');
  for(const name of ['hello-command','message-formatter']) validate(readJson('plugins/examples/'+name+'/manifest.json'),pluginSchema,name);
  validate(readJson('plugins/themes/default/theme.json'),readJson('schemas/theme.schema.json'),'default-theme');
  ok('示例manifest与主题按schema所用关键字校验；不检查未生成的JAR/签名');
  const cargo=fs.readFileSync(path.join(root,'Cargo.toml'),'utf8');
  for(const member of cargo.matchAll(/^\s*"((?:crates|apps)\/[^"\n]+)",/gm)) if(!fs.existsSync(path.join(root,member[1],'Cargo.toml'))) fail('Cargo member缺失 '+member[1]);
  const settings=fs.readFileSync(path.join(root,'settings.gradle.kts'),'utf8');
  for(const member of settings.matchAll(/include\(":([^"]+)"\)/g)) if(!fs.existsSync(path.join(root,...member[1].split(':'),'build.gradle.kts'))) fail('Gradle module缺失 '+member[1]);
  ok('Cargo/Gradle登记模块路径已检查；未编译');
  const requiredDirs=['docs/design','docs/adr','docs/protocol','apps/android','apps/desktop','apps/node','shared/ui','shared/presentation','shared/core-api','shared/core-bridge','shared/platform-api','desktop/plugin-sdk','desktop/plugin-host','desktop/platform','plugins/examples/hello-command','plugins/examples/message-formatter','plugins/themes/default','schemas/message-types','tests/protocol','tests/integration','tests/recovery','tests/fixtures','deploy/relay','tools/bindings','tools/packaging'];
  for(const p of requiredDirs) if(!fs.existsSync(path.join(root,p))) fail('缺少架构目录 '+p);
  ok('原设计要求的应用/模块/文档/部署/测试目录齐全');
  try {
    const {DatabaseSync}=require('node:sqlite');
    for(const p of ['crates/storage/migrations/0001_initial.sql','crates/node-services/migrations/0001_initial.sql']) {
      const db=new DatabaseSync(':memory:'); db.exec(fs.readFileSync(path.join(root,p),'utf8'));
      const result=db.prepare('PRAGMA integrity_check').get();
      if(Object.values(result)[0]!=='ok') fail('SQLite integrity '+p);
      if(db.prepare('PRAGMA foreign_key_check').all().length) fail('SQLite外键 '+p);
      const count=db.prepare("SELECT count(*) AS n FROM sqlite_master WHERE type='table'").get().n;
      ok(p+'：DDL执行成功，'+count+'张表，空库完整性/外键检查成功'); db.close();
    }
  }catch(e){fail('SQLite草案检查失败：'+e.message);}
  // UI 对话框自检（廉价）：每个 AlertDialog/DialogWindow 必须有关闭路径。
  // 来源：issue #1（群组详情对话框关不掉，因为 onDismissRequest 与「关闭」都是空 lambda）。
  try {
    const { execFileSync } = require('node:child_process');
    const out = execFileSync(process.execPath, [path.join(root, 'tools/check-ui-dialogs.cjs')], { encoding: 'utf8' });
    report.dialogs = out.trim();
    ok(out.trim());
  } catch (error) {
    const text = String(error.stdout || error.message || '').trim();
    fail('UI对话框自检失败：' + text);
  }
  // 文档索引一致性（非致命）：docs/ 下每个 .md 至少被 README 或某篇文档引用一次。
  try {
    const docDir = path.join(root, 'docs');
    const walkDocs = (dir, out = []) => {
      for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
        const full = path.join(dir, entry.name);
        if (entry.isDirectory()) walkDocs(full, out);
        else if (entry.name.endsWith('.md')) out.push(full);
      }
      return out;
    };
    const docs = walkDocs(docDir);
    const corpus = docs.map((file) => fs.readFileSync(file, 'utf8')).join('\n') + fs.readFileSync(path.join(root, 'README.md'), 'utf8');
    const unreferenced = docs.filter((file) => {
      const relative = path.relative(root, file).replace(/\\/g, '/');
      const base = path.basename(file);
      return !corpus.includes(relative) && !corpus.includes(base);
    }).map((file) => path.relative(root, file).replace(/\\/g, '/'));
    report.unreferencedDocs = unreferenced;
    if (unreferenced.length === 0) ok('文档索引：' + docs.length + ' 篇文档都被引用');
    else ok('文档索引：' + docs.length + ' 篇，' + unreferenced.length + ' 篇未被引用（非致命）：' + unreferenced.slice(0, 6).join(', '));
  } catch (error) {
    ok('文档索引检查跳过：' + error.message);
  }
  report.fileCount=files.length;
  report.functionCount=catalog.count;
  report.markdownLinks=links;
  fs.writeFileSync(path.join(root,'docs/validation-latest.json'),JSON.stringify(report,null,2)+'\n');
  console.log(JSON.stringify(report,null,2));
  if(report.status!=='PASS') process.exitCode=1;
})();
