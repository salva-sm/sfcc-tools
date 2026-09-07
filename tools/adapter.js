// Sits between the editor and `b2c debug`, which answers `initialize` but never sends the
// `initialized` event the Debug Adapter Protocol requires. Editors that follow the protocol
// wait for it forever, so this synthesises it once the initialize response goes past.
//
// Every argument is forwarded to the adapter untouched. Set B2C_DAP_LOG to record the
// conversation to a file.
const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const ENTRY = process.env.B2C_ADAPTER_ENTRY
    || path.join(os.homedir(), 'AppData/Local/Volta/tools/image/packages/@salesforce/b2c-cli/node_modules/@salesforce/b2c-cli/bin/run.js');

const log = process.env.B2C_DAP_LOG ? fs.createWriteStream(process.env.B2C_DAP_LOG, { flags: 'w' }) : null;
const note = (text) => log && log.write(`[${new Date().toISOString().slice(11, 23)}] ${text}\n`);

const child = spawn(process.execPath, [ENTRY, ...process.argv.slice(2)], {
    stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true,
});

let announced = false;

function send(message) {
    const body = JSON.stringify(message);
    process.stdout.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}

process.stdin.on('data', (chunk) => {
    note('editor -> adapter ' + chunk.toString().slice(0, 400));
    child.stdin.write(chunk);
});
process.stdin.on('end', () => child.stdin.end());

child.stdout.on('data', (chunk) => {
    const text = chunk.toString();
    note('adapter -> editor ' + text.slice(0, 400));
    process.stdout.write(chunk);

    // After the attach, not after the initialize: breakpoints set before the session exists
    // come back unbound.
    const attached = text.includes('"command":"attach"') || text.includes('"command":"launch"');
    if (!announced && attached && text.includes('"success":true')) {
        announced = true;
        note('injecting the initialized event the adapter never sends');
        send({ type: 'event', event: 'initialized' });
    }
});

child.stderr.on('data', (chunk) => note('adapter stderr ' + chunk.toString().trim()));
child.on('error', (error) => note('spawn error ' + error.message));
child.on('close', (code) => {
    note('adapter closed code=' + code);
    if (log) { log.end(() => process.exit(code === null ? 1 : code)); } else { process.exit(code === null ? 1 : code); }
});
process.on('SIGTERM', () => child.kill());
