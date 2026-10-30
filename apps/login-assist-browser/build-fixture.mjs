import { stripTypeScriptTypes } from 'node:module';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
await mkdir(new URL('./fixture/generated/', import.meta.url), { recursive: true });
for (const name of ['fill', 'dom-target']) {
  const source = await readFile(new URL(`./src/${name}.ts`, import.meta.url), 'utf8');
  const js = stripTypeScriptTypes(source, { mode: 'strip' }).replaceAll("'./fill.ts'", "'./fill.js'");
  await writeFile(new URL(`./fixture/generated/${name}.js`, import.meta.url), js);
}
