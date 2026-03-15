#!/usr/bin/env python3
"""Agent OS Bridge — text-based interface between LLM and Agent OS.

No tool schemas.  No prompt pollution.  The agent discovers capabilities
progressively by running ``aos`` commands.

The bridge scans LLM output for lines starting with ``$ ``, executes
them via the aos CLI, and feeds the results back.  The loop continues
until the LLM responds without any commands.

Usage:
    python3 clients/bridge/bridge.py
    python3 clients/bridge/bridge.py --container <id>
    python3 clients/bridge/bridge.py --model claude-sonnet-4-6

Requires: ANTHROPIC_API_KEY environment variable.
"""

import os
import shlex
import subprocess
import sys


# ---------------------------------------------------------------------------
# System prompt — intentionally minimal.  Everything else is discovered
# by the agent at runtime via ``$ aos``.
# ---------------------------------------------------------------------------

SYSTEM_PROMPT = """\
You are an AI agent running on Agent OS.

To interact with the system, write a command on a line starting with $:

$ aos

Run the command above to see what you can do.\
"""


# ---------------------------------------------------------------------------
# Executor — runs aos commands locally or inside a Docker container
# ---------------------------------------------------------------------------

class Executor:
    def __init__(self, container_id=None, apps_dir=None, data_dir=None):
        self.container_id = container_id
        self.apps_dir = apps_dir
        self.data_dir = data_dir

    def run(self, command_line):
        """Execute an aos command string and return output text."""
        try:
            parts = shlex.split(command_line)
        except ValueError as e:
            return f"error: invalid command syntax: {e}"

        # Strip leading "aos" — user writes "$ aos fs ls" but we call
        # the binary directly.
        if parts and parts[0] == "aos":
            parts = parts[1:]

        if self.container_id:
            cmd = ["docker", "exec", self.container_id, "aos"] + parts
        else:
            cmd = [sys.executable, self._aos_path()] + parts

        env = os.environ.copy()
        if self.apps_dir:
            env["AOS_APPS_DIR"] = self.apps_dir
        if self.data_dir:
            env["AOS_DATA_DIR"] = self.data_dir

        try:
            result = subprocess.run(
                cmd, capture_output=True, text=True, env=env, timeout=300,
            )
            output = result.stdout.strip()
            if result.stderr.strip():
                err = result.stderr.strip()
                output = f"{output}\n{err}" if output else err
            return output or "(no output)"
        except subprocess.TimeoutExpired:
            return "error: command timed out (300s)"
        except Exception as e:
            return f"error: {e}"

    def _aos_path(self):
        base = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
        return os.path.join(base, "rootfs", "overlay", "usr", "local", "bin", "aos")


# ---------------------------------------------------------------------------
# Command extraction — pull ``$ aos …`` lines from LLM text
# ---------------------------------------------------------------------------

def extract_commands(text):
    """Return command strings found in the LLM's output.

    Only lines that begin with ``$ `` are treated as commands.
    The ``$ `` prefix is stripped before returning.
    """
    commands = []
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("$ "):
            commands.append(stripped[2:])
    return commands


def format_results(results):
    """Format [(command, output), …] pairs into text for the LLM."""
    parts = []
    for cmd, output in results:
        parts.append(f"$ {cmd}\n{output}")
    return "\n\n".join(parts)


# ---------------------------------------------------------------------------
# Agent loop — pure text conversation
# ---------------------------------------------------------------------------

def run_agent(model="claude-sonnet-4-6", container_id=None, system_prompt=None):
    """Main agent loop: text-based conversation with command extraction."""
    try:
        import anthropic
    except ImportError:
        print("Error: pip install anthropic", file=sys.stderr)
        sys.exit(1)

    client = anthropic.Anthropic()

    if container_id:
        executor = Executor(container_id=container_id)
    else:
        base = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
        executor = Executor(
            apps_dir=os.path.join(base, "apps"),
            data_dir=os.environ.get("AOS_DATA_DIR", "/tmp/aos-data"),
        )

    if system_prompt is None:
        system_prompt = SYSTEM_PROMPT

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

        # Keep looping until the LLM stops issuing commands.
        while True:
            response = client.messages.create(
                model=model,
                max_tokens=4096,
                system=system_prompt,
                messages=messages,
            )

            assistant_text = "".join(
                block.text for block in response.content
                if hasattr(block, "text")
            )
            messages.append({"role": "assistant", "content": assistant_text})

            commands = extract_commands(assistant_text)
            if not commands:
                # No commands — print the final response and wait for
                # the next human message.
                print(f"\nagent> {assistant_text}\n")
                break

            # Execute every command and collect results.
            results = []
            for cmd in commands:
                print(f"  [$ {cmd}]")
                output = executor.run(cmd)
                results.append((cmd, output))

            # Feed the results back as a follow-up user message so the
            # LLM can inspect them and decide what to do next.
            messages.append({"role": "user", "content": format_results(results)})


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main():
    import argparse
    parser = argparse.ArgumentParser(description="Agent OS Bridge")
    parser.add_argument("--container", help="Docker container ID to run commands in")
    parser.add_argument("--model", default="claude-sonnet-4-6", help="Model to use")
    parser.add_argument("--system", help="Custom system prompt")
    args = parser.parse_args()

    run_agent(model=args.model, container_id=args.container, system_prompt=args.system)


if __name__ == "__main__":
    main()
