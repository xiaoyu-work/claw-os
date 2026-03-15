"""sys — System information: hardware, OS, environment, resources."""

import os
import platform
import shutil


def cmd_info(args):
    """Show system information."""
    info = {
        "name": "agent-os",
        "version": os.environ.get("AOS_VERSION", "unknown"),
        "platform": platform.platform(),
        "arch": platform.machine(),
        "python": platform.python_version(),
        "hostname": platform.node(),
        "pid": os.getpid(),
        "uid": os.getuid(),
        "shell": os.environ.get("SHELL", "/bin/bash"),
        "workspace": os.environ.get("WORKSPACE", "/workspace"),
    }
    return info


def cmd_env(args):
    """Show environment variables. Use --filter to search."""
    env = dict(os.environ)

    # Optional filter
    if args:
        pattern = args[0].lower()
        env = {k: v for k, v in env.items() if pattern in k.lower()}

    return {"env": env, "count": len(env)}


def cmd_resources(args):
    """Show disk, memory, and CPU usage."""
    result = {}

    # Disk usage
    try:
        workspace = os.environ.get("WORKSPACE", "/workspace")
        usage = shutil.disk_usage(workspace)
        result["disk"] = {
            "path": workspace,
            "total_mb": usage.total // (1024 * 1024),
            "used_mb": usage.used // (1024 * 1024),
            "free_mb": usage.free // (1024 * 1024),
            "percent_used": round(usage.used / usage.total * 100, 1),
        }
    except Exception:
        result["disk"] = {"error": "could not read disk usage"}

    # Memory (from /proc/meminfo on Linux)
    try:
        meminfo = {}
        with open("/proc/meminfo") as f:
            for line in f:
                parts = line.split(":")
                if len(parts) == 2:
                    key = parts[0].strip()
                    val = parts[1].strip().split()[0]
                    meminfo[key] = int(val)

        total = meminfo.get("MemTotal", 0)
        available = meminfo.get("MemAvailable", 0)
        used = total - available
        result["memory"] = {
            "total_mb": total // 1024,
            "used_mb": used // 1024,
            "available_mb": available // 1024,
            "percent_used": round(used / total * 100, 1) if total else 0,
        }
    except Exception:
        result["memory"] = {"error": "could not read memory info"}

    # CPU count
    try:
        result["cpu"] = {
            "count": os.cpu_count() or 0,
        }
        # Load average (Linux)
        load = os.getloadavg()
        result["cpu"]["load_avg"] = {
            "1min": round(load[0], 2),
            "5min": round(load[1], 2),
            "15min": round(load[2], 2),
        }
    except Exception:
        result["cpu"] = {"count": os.cpu_count() or 0}

    return result


def cmd_uptime(args):
    """Show system uptime."""
    try:
        with open("/proc/uptime") as f:
            seconds = float(f.read().split()[0])
        days = int(seconds // 86400)
        hours = int((seconds % 86400) // 3600)
        minutes = int((seconds % 3600) // 60)
        return {
            "uptime_seconds": int(seconds),
            "formatted": f"{days}d {hours}h {minutes}m",
        }
    except Exception:
        return {"error": "could not read uptime"}


COMMANDS = {
    "info": cmd_info,
    "env": cmd_env,
    "resources": cmd_resources,
    "uptime": cmd_uptime,
}


def run(command, args):
    """Entry point called by aos."""
    handler = COMMANDS.get(command)
    if handler is None:
        return {"error": f"unknown command: {command}"}
    return handler(args)
