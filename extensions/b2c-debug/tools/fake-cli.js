// Temporary: stands in for `b2c debug cli --rpc` so the adapter can be driven without an
// instance. Answers the RPC subset the adapter uses, with a frame that has both a plain
// pdict and a dw-style object carrying engine noise.
const path = require('path');

const reply = (id, result) => process.stdout.write(JSON.stringify({ id, result }) + '\n');

// The adapter forwards its own arguments, so the frame can sit in the tree the
// driver built.
const at = process.argv.indexOf('--cartridge-path');
const CARTRIDGES = at === -1 ? 'cartridges' : process.argv[at + 1];
const FRAME_FILE = path.join(CARTRIDGES, 'app_brand', 'cartridge', 'controllers', 'Account.js');

process.stdout.write(JSON.stringify({ event: 'ready' }) + '\n');

const GLOBALS = {
    pdict: { kind: 'object', text: '[object Object]', json: '{"CurrentCustomer":"anonymous","Order":"00012345"}' },
    request: { kind: 'object', text: 'dw.system.Request@3f21' },
    session: { kind: 'object', text: 'dw.system.Session@11ac' },
    customer: { kind: 'object', text: 'dw.customer.Customer@77be' },
    response: { kind: 'object', text: 'dw.system.Response@0a4c' },
    req: { kind: 'object', text: '[object Object]', json: '{"locale":"fr_FR","querystring":{}}' },
    viewData: { kind: 'object', text: '[object Object]', json: '{"actionUrl":"/on/demandware.store/Account-Show"}' },
    server: { kind: 'object', text: '[object Object]', json: '{"routes":12}' },
};

const MEMBERS = {
    local: [
        { name: 'req', type: 'object', value: '[object Object]', has_children: true },
        { name: 'viewData', type: 'object', value: '[object Object]', has_children: true },
        { name: 'callback', type: 'function', value: 'function () {}', has_children: false },
    ],
    closure: [{ name: 'server', type: 'object', value: '[object Object]', has_children: true }],
    'pdict': [
        { name: 'CurrentCustomer', type: 'string', value: 'anonymous', has_children: false },
        { name: '__proto__', type: 'object', value: '[object Object]', has_children: true },
    ],
    'session': [
        { name: 'currency', type: 'object', value: 'EUR', has_children: true },
        { name: 'getCurrency', type: 'function', value: 'function () {}', has_children: false },
        { name: 'class', type: 'object', value: 'dw.system.Session', has_children: false },
        { name: 'hasOwnProperty', type: 'function', value: 'function () {}', has_children: false },
    ],
};

let buffer = '';
process.stdin.on('data', (chunk) => {
    buffer += chunk.toString();
    const lines = buffer.split('\n');
    buffer = lines.pop();

    for (const line of lines.filter(Boolean)) {
        const message = JSON.parse(line);
        const args = message.args || {};

        if (message.command === 'list_threads') {
            reply(message.id, { threads: [{ thread_id: 7, status: 'halted' }] });
        } else if (message.command === 'get_stack') {
            reply(message.id, { frames: [{ index: 0, function_name: 'show', line: 42, file: FRAME_FILE }] });
        } else if (message.command === 'get_variables') {
            const key = args.object_path || args.scope;
            reply(message.id, { variables: MEMBERS[key] || [] });
        } else if (message.command === 'evaluate') {
            const typeOf = /^typeof (\w+)$/.exec(args.expression);
            const stringOf = /^String\((\w+)\)$/.exec(args.expression);
            const jsonOf = /^JSON\.stringify\((\w+)\)$/.exec(args.expression);
            if (typeOf) {
                reply(message.id, { result: GLOBALS[typeOf[1]] ? GLOBALS[typeOf[1]].kind : 'undefined' });
            } else if (stringOf) {
                reply(message.id, { result: GLOBALS[stringOf[1]] ? GLOBALS[stringOf[1]].text : 'undefined' });
            } else if (jsonOf) {
                reply(message.id, { result: (GLOBALS[jsonOf[1]] || {}).json || 'undefined' });
            } else {
                reply(message.id, { result: 'ok' });
            }
        } else {
            reply(message.id, {});
        }
    }
});
