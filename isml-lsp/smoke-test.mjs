// End-to-end check against a real SFCC checkout:
//   node smoke-test.mjs <workspace-root> <file.isml> <"needle" ...>
// For every needle, asks the server for the definition at the first column of
// that needle in the file and prints what came back.
import { spawn } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';

const [root, file, ...needles] = process.argv.slice(2);
if (!file) {
    console.error('usage: node smoke-test.mjs <workspace-root> <file> <needle>...');
    process.exit(2);
}

const rootUri = pathToFileURL(resolve(root)).href;
const fileUri = pathToFileURL(resolve(file)).href;
const text = readFileSync(file, 'utf8');
const lines = text.split(/\r?\n/);

const server = spawn('isml-lsp', { stdio: ['pipe', 'pipe', 'inherit'] });
let pending = '';
const waiting = new Map();

server.stdout.on('data', chunk => {
    pending += chunk.toString('binary');
    for (;;) {
        const headerEnd = pending.indexOf('\r\n\r\n');
        if (headerEnd < 0) return;
        const length = Number(/Content-Length: (\d+)/.exec(pending)[1]);
        const start = headerEnd + 4;
        if (pending.length < start + length) return;
        const body = Buffer.from(pending.slice(start, start + length), 'binary').toString('utf8');
        pending = pending.slice(start + length);
        const message = JSON.parse(body);
        if (waiting.has(message.id)) {
            waiting.get(message.id)(message);
            waiting.delete(message.id);
        }
    }
});

function send(message) {
    const body = Buffer.from(JSON.stringify(message), 'utf8');
    server.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
    server.stdin.write(body);
}

let nextId = 1;
function request(method, params) {
    const id = nextId++;
    return new Promise(done => {
        waiting.set(id, done);
        send({ jsonrpc: '2.0', id, method, params });
    });
}

function locate(needle) {
    const line = lines.findIndex(text => text.includes(needle));
    return line < 0 ? null : { line, character: lines[line].indexOf(needle) + 1 };
}

await request('initialize', {
    processId: process.pid,
    rootUri,
    workspaceFolders: [{ uri: rootUri, name: 'root' }],
    capabilities: {},
});
send({ jsonrpc: '2.0', method: 'initialized', params: {} });
send({
    jsonrpc: '2.0',
    method: 'textDocument/didOpen',
    params: { textDocument: { uri: fileUri, languageId: 'isml', version: 1, text } },
});

let failures = 0;
for (const needle of needles) {
    const position = locate(needle);
    if (!position) {
        console.log(`MISS  ${needle} — not present in the file`);
        failures++;
        continue;
    }
    const response = await request('textDocument/definition', {
        textDocument: { uri: fileUri },
        position,
    });
    const hits = response.result ?? [];
    if (!hits.length) {
        console.log(`FAIL  ${needle}`);
        failures++;
        continue;
    }
    console.log(`OK    ${needle}`);
    for (const hit of hits) {
        console.log(`        ${decodeURIComponent(hit.uri).replace('file:///', '')}:${hit.range.start.line + 1}`);
    }
}

await request('shutdown', null);
send({ jsonrpc: '2.0', method: 'exit' });
server.stdin.end();
process.exit(failures ? 1 : 0);
