// Speaks DAP to the editor and JSONL to `b2c debug cli --rpc`, because the CLI's own DAP
// adapter never emits `initialized` and binds no breakpoints. Arguments are forwarded as
// given; both conversations are recorded in B2C_DAP_LOG, or b2c-dap.log in the temp dir.
const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

const ENTRY = process.env.B2C_ADAPTER_ENTRY
    || path.join(os.homedir(), 'AppData/Local/Volta/tools/image/packages/@salesforce/b2c-cli/node_modules/@salesforce/b2c-cli/bin/run.js');

const log = fs.createWriteStream(process.env.B2C_DAP_LOG || path.join(os.tmpdir(), 'b2c-dap.log'), { flags: 'w' });
const note = (text) => log.write(`[${new Date().toISOString().slice(11, 23)}] ${text}\n`);

const forwarded = process.argv.slice(2).filter((argument) => argument !== 'debug');
const cli = spawn(process.execPath, [ENTRY, 'debug', 'cli', '--rpc', ...forwarded], {
    stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true,
});

let ready = false;
const readyWaiters = [];
const pending = new Map();
let rpcId = 0;

function rpc(command, args) {
    return new Promise((resolve, reject) => {
        rpcId += 1;
        pending.set(rpcId, { resolve, reject });
        const line = JSON.stringify({ id: rpcId, command, args: args || {} });
        note('-> cli ' + line);
        cli.stdin.write(line + '\n');
    });
}

function whenReady() {
    return ready ? Promise.resolve() : new Promise((resolve) => readyWaiters.push(resolve));
}

function emit(message) {
    const body = JSON.stringify(message);
    note('-> editor ' + body);
    process.stdout.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);
}

let outgoingSeq = 0;
const respond = (request, body) =>
    emit({ seq: (outgoingSeq += 1), type: 'response', request_seq: request.seq, success: true, command: request.command, body });
const fail = (request, message) =>
    emit({ seq: (outgoingSeq += 1), type: 'response', request_seq: request.seq, success: false, command: request.command, message });
const event = (name, body) =>
    emit({ seq: (outgoingSeq += 1), type: 'event', event: name, body });

const LOGGER = process.env.B2C_LOGGER || 'prost';
let logger = null;

function argumentValue(name) {
    const at = forwarded.indexOf(name);
    return at === -1 ? null : forwarded[at + 1];
}

function followLogs(configuration) {
    if (logger || configuration.logs === false) return;

    const args = ['logger', '--color', 'always', '--level', configuration.log_level || 'error,customerror'];
    const config = argumentValue('--config');
    if (config) args.push('--config', config);

    logger = spawn(LOGGER, args, { stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
    logger.stdout.on('data', (chunk) => event('output', { category: 'stdout', output: chunk.toString() }));
    logger.stderr.on('data', (chunk) => note('logger stderr ' + chunk.toString().trim()));
    logger.on('error', (error) => {
        note('logger spawn error ' + error.message);
        event('output', { category: 'console', output: `sandbox log unavailable: ${error.message}\n` });
        logger = null;
    });
}

function stopLogs() {
    if (!logger) return;
    logger.kill();
    logger = null;
}

const CARTRIDGES = 'cartridges';

/// The RPC side wants a cartridge-relative path; editors send absolute local ones.
function toScriptPath(local) {
    if (!local) return local;
    const parts = local.replace(/\\/g, '/').split('/');
    const at = parts.lastIndexOf(CARTRIDGES);
    return at === -1 ? local : parts.slice(at + 1).join('/');
}

let handles = new Map();
let nextHandle = 1;

function handleFor(descriptor) {
    const reference = nextHandle;
    nextHandle += 1;
    handles.set(reference, descriptor);
    return reference;
}

const FRAME_STRIDE = 1000;
const frameId = (threadId, index) => threadId * FRAME_STRIDE + index;
const frameParts = (id) => ({ threadId: Math.floor(id / FRAME_STRIDE), index: id % FRAME_STRIDE });

const breakpointsBySource = new Map();

const handlers = {
    initialize: (request) => respond(request, {
        supportsConfigurationDoneRequest: true,
        supportsConditionalBreakpoints: true,
        supportsEvaluateForHovers: true,
        supportsTerminateRequest: true,
        supportsLogPoints: true,
    }),

    attach: async (request) => {
        await whenReady();
        followLogs(request.arguments || {});
        respond(request, {});
        event('initialized', {});
    },

    launch: async (request) => {
        await whenReady();
        respond(request, {});
        event('initialized', {});
    },

    setBreakpoints: async (request) => {
        const source = request.arguments.source || {};
        const wanted = request.arguments.breakpoints || [];
        breakpointsBySource.set(source.path, wanted.map((point) => ({
            file: toScriptPath(source.path),
            line: point.line,
            ...(point.condition ? { condition: point.condition } : {}),
        })));

        const all = [...breakpointsBySource.values()].flat();
        const result = await rpc('set_breakpoints', { breakpoints: all }).catch((error) => ({ error }));
        if (result.error) return fail(request, String(result.error));

        const bound = result.breakpoints || [];
        respond(request, {
            breakpoints: wanted.map((point) => {
                const match = bound.find((candidate) =>
                    candidate.line === point.line && toScriptPath(candidate.file) === toScriptPath(source.path));
                return match
                    ? { id: match.id, verified: true, line: match.line, source }
                    : { verified: false, line: point.line, source, message: 'the instance did not bind this line' };
            }),
        });
    },

    configurationDone: (request) => respond(request, {}),

    threads: async (request) => {
        const result = await rpc('list_threads').catch(() => ({ threads: [] }));
        respond(request, {
            threads: (result.threads || []).map((thread) => ({
                id: thread.thread_id,
                name: `Request thread ${thread.thread_id}${thread.status ? ' (' + thread.status + ')' : ''}`,
            })),
        });
    },

    stackTrace: async (request) => {
        const threadId = request.arguments.threadId;
        const result = await rpc('get_stack', { thread_id: threadId }).catch(() => ({ frames: [] }));
        const frames = (result.frames || []).map((frame) => ({
            id: frameId(threadId, frame.index),
            name: frame.function_name || '(anonymous)',
            line: frame.line,
            column: 1,
            source: { name: path.basename(frame.file || frame.script_path || ''), path: frame.file },
        }));
        respond(request, { stackFrames: frames, totalFrames: frames.length });
    },

    scopes: (request) => {
        const { threadId, index } = frameParts(request.arguments.frameId);
        respond(request, {
            scopes: [
                { name: 'Locals', variablesReference: handleFor({ threadId, index, scope: 'local' }), expensive: false },
                { name: 'Closure', variablesReference: handleFor({ threadId, index, scope: 'closure' }), expensive: false },
            ],
        });
    },

    variables: async (request) => {
        const descriptor = handles.get(request.arguments.variablesReference);
        if (!descriptor) return respond(request, { variables: [] });

        const result = await rpc('get_variables', {
            thread_id: descriptor.threadId,
            frame_index: descriptor.index,
            ...(descriptor.scope ? { scope: descriptor.scope } : {}),
            ...(descriptor.objectPath ? { object_path: descriptor.objectPath } : {}),
        }).catch((error) => ({ variables: [], failed: String(error) }));

        if (result.failed) {
            return respond(request, {
                variables: [{ name: '<unavailable>', value: result.failed, variablesReference: 0 }],
            });
        }

        const raw = result.variables || [];
        const paths = raw.map((variable) =>
            descriptor.objectPath ? `${descriptor.objectPath}.${variable.name}` : variable.name);
        const summaries = await describeAll(raw, paths, descriptor);

        respond(request, {
            variables: raw.map((variable, position) => ({
                name: variable.name,
                value: oneLine(summaries[position] ?? String(variable.value ?? '')),
                type: variable.type,
                evaluateName: paths[position],
                variablesReference: variable.has_children
                    ? handleFor({ threadId: descriptor.threadId, index: descriptor.index, objectPath: paths[position] })
                    : 0,
            })),
        });
    },

    evaluate: async (request) => {
        const { threadId, index } = request.arguments.frameId
            ? frameParts(request.arguments.frameId)
            : { threadId: undefined, index: undefined };
        const result = await rpc('evaluate', {
            expression: request.arguments.expression,
            ...(threadId === undefined ? {} : { thread_id: threadId, frame_index: index }),
        }).catch((error) => ({ failed: String(error) }));

        const value = String(result.failed ?? result.result ?? '');
        // A hover over something that is not in scope should show nothing, not an error string.
        if (result.failed || /^(Reference|Type|Syntax)Error\b/.test(value)) {
            return fail(request, value);
        }
        respond(request, { result: value, variablesReference: 0 });
    },

    continue: async (request) => {
        await rpc('continue', { thread_id: request.arguments.threadId }).catch(() => {});
        respond(request, { allThreadsContinued: false });
    },

    next: (request) => step(request, 'step_over'),
    stepIn: (request) => step(request, 'step_into'),
    stepOut: (request) => step(request, 'step_out'),

    pause: (request) => fail(request, 'the script debugger cannot pause a running request; it only halts on breakpoints'),

    disconnect: (request) => {
        respond(request, {});
        stopLogs();
        cli.stdin.end();
        setTimeout(() => process.exit(0), 300);
    },
    terminate: (request) => handlers.disconnect(request),
};

const OPAQUE = '[object Object]';
const DESCRIBE_LIMIT = 24;
const SUMMARY_LENGTH = 240;

/// `[object Object]` tells nobody anything. dw classes are Java-backed and answer `String()`
/// with something readable; plain objects only answer `JSON.stringify`.
async function describeAll(variables, paths, descriptor) {
    if (process.env.B2C_DESCRIBE_OBJECTS === 'off') return variables.map(() => null);

    let budget = DESCRIBE_LIMIT;
    return Promise.all(variables.map((variable, position) => {
        if (String(variable.value) !== OPAQUE || budget <= 0) return null;
        budget -= 1;
        return describe(paths[position], descriptor);
    }));
}

async function describe(expression, descriptor) {
    const ask = (text) => rpc('evaluate', {
        expression: text,
        thread_id: descriptor.threadId,
        frame_index: descriptor.index,
    }).then((answer) => String(answer.result ?? '')).catch(() => '');

    const readable = await ask(`String(${expression})`);
    if (readable && readable !== OPAQUE && !/Error\b/.test(readable)) {
        return oneLine(readable);
    }

    const serialised = await ask(`JSON.stringify(${expression})`);
    if (serialised && serialised !== '{}' && serialised !== 'undefined' && !/Error\b/.test(serialised)) {
        return oneLine(serialised);
    }
    return null;
}

function oneLine(text) {
    const flat = text.replace(/\s+/g, ' ').trim();
    return flat.length > SUMMARY_LENGTH ? flat.slice(0, SUMMARY_LENGTH - 1) + '…' : flat;
}

async function step(request, command) {
    await rpc(command, { thread_id: request.arguments.threadId }).catch(() => {});
    respond(request, {});
}

let incoming = Buffer.alloc(0);
process.stdin.on('data', (chunk) => {
    incoming = Buffer.concat([incoming, chunk]);
    for (;;) {
        const header = incoming.indexOf('\r\n\r\n');
        if (header === -1) return;
        const length = Number(/Content-Length: (\d+)/i.exec(incoming.slice(0, header).toString())?.[1]);
        if (!length || incoming.length < header + 4 + length) return;

        const body = incoming.slice(header + 4, header + 4 + length).toString();
        incoming = incoming.slice(header + 4 + length);
        note('<- editor ' + body);

        let request;
        try { request = JSON.parse(body); } catch { continue; }
        const handler = handlers[request.command];
        if (handler) {
            Promise.resolve(handler(request)).catch((error) => fail(request, String(error)));
        } else {
            note('unhandled request ' + request.command);
            respond(request, {});
        }
    }
});

let cliBuffer = '';
cli.stdout.on('data', (chunk) => {
    cliBuffer += chunk.toString();
    const lines = cliBuffer.split('\n');
    cliBuffer = lines.pop();

    for (const line of lines.filter(Boolean)) {
        note('<- cli ' + line);
        let message;
        try { message = JSON.parse(line); } catch { continue; }

        if (message.event === 'ready') {
            ready = true;
            readyWaiters.splice(0).forEach((resolve) => resolve());
            continue;
        }
        if (message.event === 'thread_stopped') {
            handles = new Map();
            event('thread', { reason: 'started', threadId: message.data.thread_id });
            event('stopped', {
                reason: 'breakpoint',
                threadId: message.data.thread_id,
                allThreadsStopped: false,
                preserveFocusHint: false,
            });
            continue;
        }
        if (message.id && pending.has(message.id)) {
            const { resolve, reject } = pending.get(message.id);
            pending.delete(message.id);
            message.error ? reject(message.error) : resolve(message.result || {});
        }
    }
});

cli.stderr.on('data', (chunk) => {
    const text = chunk.toString().trim();
    note('cli stderr ' + text);
    if (/^ERROR/.test(text)) event('output', { category: 'stderr', output: text + '\n' });
});

cli.on('error', (error) => { note('cli spawn error ' + error.message); process.exit(1); });
cli.on('close', (code) => {
    note('cli closed code=' + code);
    event('terminated', {});
    if (log) { log.end(() => process.exit(0)); } else { process.exit(0); }
});
