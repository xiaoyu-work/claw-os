"""Agent OS Bridge — connects an LLM to Agent OS.

This is the missing piece: it translates LLM tool calls into aos commands,
either locally or inside a Docker container.

Usage:
    # Run locally (for development)
    python3 agent/bridge.py

    # Run against a container
    python3 agent/bridge.py --container <id>

    # With a specific model
    python3 agent/bridge.py --model claude-sonnet-4-6

Requires: ANTHROPIC_API_KEY environment variable.
"""

import json
import os
import subprocess
import sys

# ---------------------------------------------------------------------------
# Tool → aos command mapping
# ---------------------------------------------------------------------------

def tool_to_aos(tool_name, tool_input):
    """Convert a tool call into an aos CLI command (argv list)."""
    mapping = {
        "fs_ls":       lambda i: ["fs", "ls"] + ([i["path"]] if i.get("path") else []),
        "fs_read":     lambda i: ["fs", "read", i["path"]],
        "fs_write":    lambda i: ["fs", "write", "--content", i["content"], i["path"]],
        "fs_search":   lambda i: ["fs", "search", i["query"]] + ([i["path"]] if i.get("path") else []),
        "exec_run":    lambda i: ["exec", "run", "--shell", i["command"]],
        "exec_script": lambda i: ["exec", "script", "--lang", i.get("lang", "python"), i["code"]],
        "kv_get":      lambda i: ["kv", "get", i["key"]],
        "kv_set":      lambda i: ["kv", "set", i["key"], i["value"]],
        "web_read":    lambda i: ["web", "read", i["url"]],
        "net_fetch":   lambda i: _build_net_fetch(i),
        "doc_read":    lambda i: ["doc", "read", i["path"]],
        "pkg_need":    lambda i: ["pkg", "need"] + i["packages"],
        "notify_send": lambda i: ["notify", "send"] + (["--urgent"] if i.get("urgent") else []) + [i["message"]],
    }
    builder = mapping.get(tool_name)
    if builder is None:
        return None
    return builder(tool_input)


def _build_net_fetch(i):
    args = ["net", "fetch"]
    if i.get("method"):
        args += ["--method", i["method"]]
    if i.get("data"):
        args += ["--data", i["data"]]
    args.append(i["url"])
    return args


# ---------------------------------------------------------------------------
# Executor — runs aos commands locally or in a container
# ---------------------------------------------------------------------------

class Executor:
    def __init__(self, container_id=None, apps_dir=None, data_dir=None):
        self.container_id = container_id
        self.apps_dir = apps_dir
        self.data_dir = data_dir

    def run_aos(self, aos_args):
        """Execute an aos command and return parsed JSON result."""
        if self.container_id:
            cmd = ["docker", "exec", self.container_id, "aos"] + aos_args
        else:
            cmd = [sys.executable, self._aos_path()] + aos_args

        env = os.environ.copy()
        if self.apps_dir:
            env["AOS_APPS_DIR"] = self.apps_dir
        if self.data_dir:
            env["AOS_DATA_DIR"] = self.data_dir

        result = subprocess.run(cmd, capture_output=True, text=True, env=env, timeout=300)
        stdout = result.stdout.strip()
        if stdout:
            try:
                return json.loads(stdout)
            except json.JSONDecodeError:
                return {"output": stdout}
        if result.stderr:
            return {"error": result.stderr.strip()}
        return {"error": f"aos exited with code {result.returncode}"}

    def _aos_path(self):
        base = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        return os.path.join(base, "rootfs", "overlay", "usr", "local", "bin", "aos")


# ---------------------------------------------------------------------------
# Agent loop — LLM ↔ Agent OS conversation
# ---------------------------------------------------------------------------

def load_tools():
    tools_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "tools.json")
    with open(tools_path) as f:
        return json.load(f)


def run_agent(model="claude-sonnet-4-6", container_id=None, system_prompt=None):
    """Main agent loop: user talks to LLM, LLM uses Agent OS tools."""
    try:
        import anthropic
    except ImportError:
        print("Error: pip install anthropic", file=sys.stderr)
        sys.exit(1)

    client = anthropic.Anthropic()
    tools = load_tools()

    # Determine executor
    if container_id:
        executor = Executor(container_id=container_id)
    else:
        base = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        executor = Executor(
            apps_dir=os.path.join(base, "apps"),
            data_dir=os.environ.get("AOS_DATA_DIR", "/tmp/aos-data"),
        )

    if system_prompt is None:
        system_prompt = (
            "You are an AI agent running on Agent OS. "
            "You have access to a full operating system through the provided tools. "
            "You can read/write files, execute code, browse the web, manage packages, "
            "and remember things using the key-value store. "
            "Your workspace is at /workspace. "
            "Always use the tools to interact with the system — do not guess or make up results."
        )

    messages = []
    print("Agent OS — Type your request (Ctrl+C to exit)\n")

    while True:
        try:
            user_input = input("you> ").strip()
        except (KeyboardInterrupt, EOFError):
            print("\nbye")
            break
        if not user_input:
            continue

        messages.append({"role": "user", "content": user_input})

        # Agent loop: keep going until LLM stops calling tools
        while True:
            response = client.messages.create(
                model=model,
                max_tokens=4096,
                system=system_prompt,
                tools=tools,
                messages=messages,
            )

            # Collect assistant response
            assistant_content = response.content
            messages.append({"role": "assistant", "content": assistant_content})

            # Check if there are tool calls
            tool_uses = [b for b in assistant_content if b.type == "tool_use"]
            if not tool_uses:
                # No tool calls — print text response and break to next user input
                for block in assistant_content:
                    if hasattr(block, "text"):
                        print(f"\nagent> {block.text}\n")
                break

            # Execute all tool calls
            tool_results = []
            for tool_use in tool_uses:
                aos_args = tool_to_aos(tool_use.name, tool_use.input)
                if aos_args is None:
                    result = {"error": f"unknown tool: {tool_use.name}"}
                else:
                    print(f"  [aos {' '.join(aos_args[:3])}{'...' if len(aos_args) > 3 else ''}]")
                    result = executor.run_aos(aos_args)

                tool_results.append({
                    "type": "tool_result",
                    "tool_use_id": tool_use.id,
                    "content": json.dumps(result),
                })

            messages.append({"role": "user", "content": tool_results})
            # Loop back to let LLM process tool results


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main():
    import argparse
    parser = argparse.ArgumentParser(description="Agent OS Bridge")
    parser.add_argument("--container", help="Docker container ID to run commands in")
    parser.add_argument("--model", default="claude-sonnet-4-6", help="Claude model to use")
    parser.add_argument("--system", help="Custom system prompt")
    args = parser.parse_args()

    run_agent(model=args.model, container_id=args.container, system_prompt=args.system)


if __name__ == "__main__":
    main()
