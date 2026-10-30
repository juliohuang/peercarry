import { mkdir, copyFile } from 'node:fs/promises';
const here = new URL('./', import.meta.url);
await mkdir(new URL('extension/vendor/', here), { recursive:true });
await copyFile(new URL('node_modules/qrcode-generator/dist/qrcode.mjs', here), new URL('extension/vendor/qrcode.mjs', here));
// The upstream module carries its copyright and MIT license notice in its header.
