import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync } from 'node:fs';
import path from 'node:path';

const root = path.resolve(import.meta.dirname, '..');
function run(command, args) {
  const result = spawnSync(command, args, { cwd: root, stdio: 'inherit', windowsHide: true });
  if (result.status !== 0) throw new Error(`${command} failed: ${result.error || result.status}`);
}
const lock = readFileSync(path.join(root, 'Cargo.lock'), 'utf8');
const bindingVersion = lock.match(/name = "wasm-bindgen"\r?\nversion = "([^"]+)"/)[1];
const binary = process.env.WASM_BINDGEN || 'wasm-bindgen';
const version = spawnSync(binary, ['--version'], { encoding: 'utf8', windowsHide: true });
if (!version.stdout?.includes(bindingVersion)) throw new Error(`Install wasm-bindgen-cli ${bindingVersion}, or set WASM_BINDGEN to its executable`);
run('cargo', ['build', '-p', 'newphoto-web', '--target', 'wasm32-unknown-unknown', '--profile', 'wasm-release', '--locked', '-j', '2']);
mkdirSync(path.join(root, 'web/dist'), { recursive: true });
run(binary, ['--target', 'web', '--out-dir', 'web/dist/pkg', '--out-name', 'newphoto', 'target/wasm32-unknown-unknown/wasm-release/newphoto-web.wasm']);
for (const file of ['index.html', 'app.js', 'style.css', 'theme.js', 'bootstrap.js']) cpSync(path.join(root, 'web', file), path.join(root, 'web/dist', file));
console.log('Leptos CSR build complete: web/dist');
