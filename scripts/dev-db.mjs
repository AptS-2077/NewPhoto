import EmbeddedPostgres from 'embedded-postgres';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { randomBytes } from 'node:crypto';
import path from 'node:path';

const directory = path.resolve('.cache/postgres');
mkdirSync('.cache', { recursive: true });
const secretPath = '.cache/postgres-password';
if (!existsSync(secretPath)) writeFileSync(secretPath, randomBytes(24).toString('hex'), { mode: 0o600 });
const password = readFileSync(secretPath, 'utf8').trim();
const postgres = new EmbeddedPostgres({ databaseDir: directory, user: 'newphoto', password, port: 15432, persistent: true, initdbFlags: ['--encoding=UTF8', '--locale=C'], postgresFlags: ['-h', '127.0.0.1'], onLog: message => console.log(message), onError: message => console.error(message) });
if (!existsSync(path.join(directory, 'PG_VERSION'))) await postgres.initialise();
await postgres.start();
const client = postgres.getPgClient();
await client.connect();
if (!(await client.query("SELECT 1 FROM pg_database WHERE datname='newphoto'")).rowCount) await client.query('CREATE DATABASE newphoto');
await client.end();
writeFileSync('.cache/database-url', `postgresql://newphoto:${password}@127.0.0.1:15432/newphoto`, { mode: 0o600 });
console.log('Development PostgreSQL ready on 127.0.0.1:15432; private connection string: .cache/database-url');
let stopping = false;
async function stop() { if (stopping) return; stopping = true; await postgres.stop(); process.exit(0); }
process.on('SIGINT', stop);
process.on('SIGTERM', stop);
setInterval(() => {}, 60000);
