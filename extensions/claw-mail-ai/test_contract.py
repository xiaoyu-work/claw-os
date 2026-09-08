"""Execute real UI request builders against deterministic Thunderbird stubs."""

import json
from pathlib import Path
import subprocess


HERE = Path(__file__).parent
MANIFEST = json.loads((HERE.parent.parent / "apps" / "mail-ai" / "app.json").read_text())


def test_protocol_pair_version_advances_without_identity_aliases():
    extension = json.loads((HERE / "manifest.json").read_text())
    assert extension["version"] == MANIFEST["version"]
    assert tuple(map(int, extension["version"].split("."))) > (0, 1, 0)
    assert extension["browser_specific_settings"]["gecko"]["id"] == "claw-mail-ai@claw.os"
    assert MANIFEST["id"] == "mail-ai"


UI_DRIVER = r"""
const fs = require("fs");
const vm = require("vm");
const root = process.argv[1];
const requests = [];
const recent = [{id: 12, sender: "alex", subject: "Review", date: "Friday"}];
const history = {};
const noop = () => {};
function element() {
  return {
    value: "hello", textContent: "", innerHTML: "", scrollHeight: 0,
    appendChild: noop, remove: noop, addEventListener: noop,
    classList: {add: noop, remove: noop},
  };
}
function context() {
  const elements = {};
  const ui = {
    aiCall: async (verb, args) => {
      requests.push({verb, args});
      // Only chat needs rendering here, to verify the response-field contract.
      return verb === "chat" ? {ok: true, result: {answer: "Friday [1]."}} : {ok: false};
    },
    showBusy: noop, clearBusy: noop, showError: noop, el: element,
    localiseDom: noop, copyToClipboard: noop,
    getSettings: async () => ({summarize: {}, features: {}}),
    setSettings: async () => {},
  };
  const messages = {
    getBody: async () => ({plain: "Review please", from: "alex", subject: "Q3"}),
    getDisplayedMessage: async () => ({id: 12}),
    getComposeDetails: async () => ({
      to: ["alex"], subject: "Review", body: "Hello", isPlainText: true,
    }),
    bodyToPlain: text => text,
  };
  return vm.createContext({
    window: {ClawUI: ui, ClawMessages: messages}, ClawUI: ui, console,
    document: {
      addEventListener: noop,
      getElementById: id => elements[id] ||= element(),
      querySelectorAll: () => [],
    },
    browser: {
      i18n: {getUILanguage: () => "en", getMessage: () => ""},
      runtime: {
        sendMessage: async msg => msg.kind === "listRecentMessages" ? recent : null,
      },
      storage: {local: {
        get: async () => history,
        set: async value => Object.assign(history, value),
      }},
    },
  });
}
function load(ctx, path) {
  vm.runInContext(fs.readFileSync(root + "/" + path, "utf8"), ctx);
}
(async () => {
  let ctx = context();
  load(ctx, "ui/summarize/summarize.js");
  await vm.runInContext("run()", ctx);
  ctx = context();
  load(ctx, "ui/compose/composeAction.js");
  await vm.runInContext("composeTab = {id: 1}; runSmartReply()", ctx);
  await vm.runInContext('document.getElementById("compose-style").value = "short"; runSmartCompose()', ctx);
  ctx = context();
  load(ctx, "ui/translate/translate.js");
  await vm.runInContext("run()", ctx);
  ctx = context();
  load(ctx, "ui/spaces/assistant.js");
  await vm.runInContext("onSend()", ctx);
  if (history.assistantHistory.at(-1).content !== "Friday [1].") {
    throw Error("Chat UI did not consume the shared answer field");
  }
  const previous = requests.length;
  ctx.browser.runtime.sendMessage = async () => ({error: "mailbox unavailable"});
  await vm.runInContext('document.getElementById("input").value = "Again"; onSend()', ctx);
  if (requests.length !== previous) throw Error("Failed mailbox read silently reached AI");

  ctx = context();
  ctx.ClawNative = {call: async (verb, args) => {
    requests.push({verb, args}); return {ok: false};
  }};
  ctx.browser.messages = {getFull: async () => ({})};
  ctx.extractPlainText = () => "Body snippet";
  ctx.hasRealAttachments = () => true;
  const background = fs.readFileSync(root + "/background.js", "utf8");
  const triage = background.match(/async function triageMessage[\s\S]*?(?=\n(?:async )?function )/);
  if (!triage) throw Error("Missing triage handler");
  vm.runInContext(triage[0], ctx);
  await vm.runInContext('triageMessage({id: 12, author: "alex", subject: "Q3"}, {})', ctx);
  process.stdout.write(JSON.stringify(requests));
})().catch(error => { console.error(error); process.exit(1); });
"""


def test_all_ui_consumers_match_the_single_manifest():
    process = subprocess.run(
        ["node", "-e", UI_DRIVER, str(HERE.resolve())],
        capture_output=True, text=True, timeout=15, cwd=HERE,
    )
    assert process.returncode == 0, process.stderr
    requests = json.loads(process.stdout)
    by_name = {tool["name"].split(".")[1]: tool for tool in MANIFEST["mcp"]["tools"]}
    assert {request["verb"] for request in requests} == set(by_name)
    assert len(requests) == 6
    for request in requests:
        args = request["args"]
        fields = {arg["name"]: arg for arg in by_name[request["verb"]]["args"]}
        assert args.keys() <= fields.keys()
        assert {name for name, arg in fields.items() if arg["required"]} <= args.keys()
        for name, value in args.items():
            assert type(value) is (bool if fields[name]["kind"] == "bool" else str)
            if "choices" in fields[name]:
                assert value in fields[name]["choices"]
    chat = next(request["args"] for request in requests if request["verb"] == "chat")
    assert json.loads(chat["context_json"]) == [
        {"sender": "alex", "subject": "Review", "date": "Friday"},
    ]
    triage = next(request["args"] for request in requests if request["verb"] == "triage")
    assert triage["sender"] == "alex" and triage["has_attachments"] is True


def test_native_bridge_preserves_invalid_args_for_host_validation():
    script = r"""
const fs = require("fs"), vm = require("vm");
const sent = [];
const listener = {addListener() {}};
const port = {
  onMessage: listener, onDisconnect: listener,
  postMessage: message => sent.push(message),
};
const ctx = vm.createContext({
  self: {}, console, setTimeout: () => 1, clearTimeout() {},
  browser: {runtime: {connectNative: () => port}},
});
vm.runInContext(fs.readFileSync(process.argv[1], "utf8"), ctx);
ctx.self.ClawNative.connect("os.claw.mail_ai");
for (const args of [false, null, [], "", {body: true}]) {
  ctx.self.ClawNative.call("summarize", args);
}
process.stdout.write(JSON.stringify(sent.map(request => request.args)));
"""
    process = subprocess.run(
        ["node", "-e", script, str(HERE / "lib" / "native.js")],
        capture_output=True, text=True, timeout=10, cwd=HERE,
    )
    assert process.returncode == 0, process.stderr
    assert json.loads(process.stdout) == [False, None, [], "", {"body": True}]
