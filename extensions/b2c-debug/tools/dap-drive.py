"""Temporary: drives the adapter over DAP against tools/fake-cli.js."""
import json
import os
import subprocess
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
env = dict(os.environ, B2C_ADAPTER_ENTRY=os.path.join(HERE, "fake-cli.js"),
           B2C_DAP_LOG=os.path.join(HERE, "drive.log"))

# Three cartridges holding the same controller, so a frame has something to be
# overridden by.
CARTRIDGES = os.path.join(tempfile.gettempdir(), "b2c-dap-drive", "cartridges")
for cartridge in ("app_brand", "app_storefront_base", "int_payment"):
    directory = os.path.join(CARTRIDGES, cartridge, "cartridge", "controllers")
    os.makedirs(directory, exist_ok=True)
    with open(os.path.join(directory, "Account.js"), "w", encoding="utf-8") as handle:
        handle.write("// stand-in\n")

proc = subprocess.Popen(
    ["node", os.path.join(HERE, "adapter.js"), "--cartridge-path", CARTRIDGES],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env)
seq = 0


def send(command, arguments=None):
    global seq
    seq += 1
    body = json.dumps({"seq": seq, "type": "request", "command": command,
                       "arguments": arguments or {}}).encode()
    proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    proc.stdin.flush()
    return seq


def read():
    length = 0
    while True:
        line = proc.stdout.readline()
        if line in (b"\r\n", b"\n"):
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":")[1])
    return json.loads(proc.stdout.read(length))


def ask(command, arguments=None):
    wanted = send(command, arguments)
    while True:
        message = read()
        if message.get("type") == "response" and message.get("request_seq") == wanted:
            return message.get("body", {})


ask("initialize", {"adapterID": "b2c"})
ask("attach", {"logs": False})
ask("configurationDone")

frame = ask("stackTrace", {"threadId": 7})["stackFrames"][0]
print("frame:", frame["name"], frame["source"]["path"], "line", frame["line"])

scopes = ask("scopes", {"frameId": frame["id"]})["scopes"]
print("scopes:", ", ".join(scope["name"] for scope in scopes))

for scope in scopes:
    body = ask("variables", {"variablesReference": scope["variablesReference"]})
    print(f"\n{scope['name']}:")
    for variable in body["variables"]:
        expands = " (expandable)" if variable["variablesReference"] else ""
        print(f"   {variable['name']:<16} {variable.get('type', ''):<10} {variable['value']}{expands}")

sfcc = next(s for s in scopes if s["name"] == "SFCC")
members = ask("variables", {"variablesReference": sfcc["variablesReference"]})["variables"]
session = next(v for v in members if v["name"] == "session")
body = ask("variables", {"variablesReference": session["variablesReference"]})
print("\nsession expanded (filtered):")
for variable in body["variables"]:
    print(f"   {variable['name']:<16} {variable.get('type', '')}")

send("disconnect")
proc.wait(timeout=10)
