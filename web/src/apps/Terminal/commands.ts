import { useFileSystemStore, type FSNode } from '@/stores/useFileSystemStore';

export interface CommandContext {
  cwd: string;
  setCwd: (cwd: string) => void;
  history: string[];
  addToHistory: (cmd: string) => void;
}

function resolvePath(path: string, cwd: string): string {
  if (path.startsWith('/')) {
    return path === '/' ? 'fs-root' : (() => {
      const fs = useFileSystemStore.getState();
      const node = fs.getNodeByPath(path);
      return node?.id || '';
    })();
  }
  if (path === '.') return cwd;
  if (path === '..') {
    const fs = useFileSystemStore.getState();
    const node = fs.getNode(cwd);
    return node?.parentId || cwd;
  }
  const parts = path.split('/').filter(Boolean);
  let current = cwd;
  const fs = useFileSystemStore.getState();
  for (const part of parts) {
    if (part === '.') continue;
    if (part === '..') {
      const node = fs.getNode(current);
      current = node?.parentId || current;
      continue;
    }
    const children = fs.getChildren(current);
    const child = children.find((c) => c.name === part);
    if (!child) return '';
    current = child.id;
  }
  return current;
}

function formatSize(size: number): string {
  if (size < 1024) return `${size}B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)}K`;
  if (size < 1024 * 1024 * 1024) return `${(size / 1024 / 1024).toFixed(1)}M`;
  return `${(size / 1024 / 1024 / 1024).toFixed(1)}G`;
}

function formatDate(d: Date): string {
  const months = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  return `${months[d.getMonth()]} ${String(d.getDate()).padStart(2, '0')} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

const commands: Record<string, (args: string[], ctx: CommandContext) => string> = {
  help: () => `Available commands:
  ls [-la] [dir]     List directory contents
  cd [dir]           Change directory
  pwd                Print working directory
  cat [file]         Display file contents
  echo [text]        Print text
  mkdir [name]       Create directory
  touch [name]       Create empty file
  rm [-r] [name]     Remove file or directory
  cp [src] [dst]     Copy file
  mv [src] [dst]     Move file
  clear              Clear screen
  neofetch           Display system info
  whoami             Print current user
  date               Print current date/time
  cal                Display calendar
  df                 Disk free
  free               Memory usage
  ps                 List processes
  top                Display system tasks
  find [dir] [name]  Find files
  grep [pattern]     Search in files
  chmod [mode] [f]   Change permissions
  history            Command history
  uname [-a]         System information
  hostname           Print hostname
  uptime             System uptime
  wc [file]          Word count
  head [file]        Show first 10 lines
  tail [file]        Show last 10 lines
  tree [dir]         Directory tree
  du [dir]           Disk usage
  id                 User identity
  env                Environment variables
  exit               Close terminal`,

  ls: (args, ctx) => {
    const fs = useFileSystemStore.getState();
    const showAll = args.includes('-la') || args.includes('-a') || args.includes('-al');
    const showLong = args.includes('-la') || args.includes('-l') || args.includes('-al');
    const pathArg = args.find((a) => !a.startsWith('-'));
    const dirId = pathArg ? resolvePath(pathArg, ctx.cwd) : ctx.cwd;
    if (!dirId) return `ls: cannot access '${pathArg}': No such file or directory`;
    const node = fs.getNode(dirId);
    if (!node) return `ls: cannot access '${pathArg}': No such file or directory`;
    if (node.type === 'file') return node.name;
    const children = fs.getChildren(dirId);
    if (!showLong) {
      return children
        .filter((c) => showAll || !c.name.startsWith('.'))
        .map((c) => c.name)
        .join('  ') || '';
    }
    return children
      .filter((c) => showAll || !c.name.startsWith('.'))
      .map((c) => {
        const perms = c.permissions;
        const owner = c.owner.padEnd(6);
        const size = String(formatSize(c.size || 0)).padStart(8);
        const date = formatDate(c.modifiedAt);
        return `${perms} ${owner} ${size} ${date} ${c.name}`;
      })
      .join('\n');
  },

  cd: (args, ctx) => {
    if (!args[0] || args[0] === '~') { ctx.setCwd('fs-user'); return ''; }
    const fs = useFileSystemStore.getState();
    const dirId = resolvePath(args[0], ctx.cwd);
    if (!dirId) return `cd: no such file or directory: ${args[0]}`;
    const node = fs.getNode(dirId);
    if (!node) return `cd: no such file or directory: ${args[0]}`;
    if (node.type !== 'directory') return `cd: not a directory: ${args[0]}`;
    ctx.setCwd(dirId);
    return '';
  },

  pwd: (_, ctx) => {
    const fs = useFileSystemStore.getState();
    return fs.getPath(ctx.cwd) || '/';
  },

  cat: (args) => {
    if (!args[0]) return 'cat: missing file operand';
    const fs = useFileSystemStore.getState();
    const fileId = resolvePath(args[0], fs.currentDirectory);
    if (!fileId) return `cat: ${args[0]}: No such file or directory`;
    const node = fs.getNode(fileId);
    if (!node) return `cat: ${args[0]}: No such file or directory`;
    if (node.type === 'directory') return `cat: ${args[0]}: Is a directory`;
    return node.content || '';
  },

  echo: (args) => args.join(' '),

  mkdir: (args, ctx) => {
    if (!args[0]) return 'mkdir: missing operand';
    const fs = useFileSystemStore.getState();
    fs.createDirectory(args[0], ctx.cwd);
    return '';
  },

  touch: (args, ctx) => {
    if (!args[0]) return 'touch: missing file operand';
    const fs = useFileSystemStore.getState();
    fs.createFile(args[0], ctx.cwd, '');
    return '';
  },

  rm: (args, ctx) => {
    if (!args[0]) return 'rm: missing operand';
    const recursive = args.includes('-r') || args.includes('-rf') || args.includes('-fr');
    const target = args.find((a) => !a.startsWith('-'));
    if (!target) return 'rm: missing operand';
    const fs = useFileSystemStore.getState();
    const nodeId = resolvePath(target, ctx.cwd);
    if (!nodeId) return `rm: cannot remove '${target}': No such file or directory`;
    const node = fs.getNode(nodeId);
    if (!node) return `rm: cannot remove '${target}': No such file or directory`;
    if (node.type === 'directory' && !recursive) return `rm: cannot remove '${target}': Is a directory`;
    fs.deleteNode(nodeId);
    return '';
  },

  cp: (args, ctx) => {
    if (args.length < 2) return 'cp: missing file operand';
    const fs = useFileSystemStore.getState();
    const srcId = resolvePath(args[0], ctx.cwd);
    if (!srcId) return `cp: cannot stat '${args[0]}': No such file or directory`;
    const srcNode = fs.getNode(srcId);
    if (!srcNode) return `cp: cannot stat '${args[0]}': No such file or directory`;
    fs.createFile(args[1], ctx.cwd, srcNode.content || '');
    return '';
  },

  mv: (args, ctx) => {
    if (args.length < 2) return 'mv: missing file operand';
    const fs = useFileSystemStore.getState();
    const srcId = resolvePath(args[0], ctx.cwd);
    if (!srcId) return `mv: cannot stat '${args[0]}': No such file or directory`;
    fs.renameNode(srcId, args[1]);
    return '';
  },

  clear: (_, ctx) => '__CLEAR__',

  neofetch: () => {
    const user = 'user';
    const host = 'webos-desktop';
    const line = (label: string, value: string) => `\x1b[1;35m${label}\x1b[0m: ${value}`;
    return [
      '        .-/+oossssoo+/-.               ' + `\x1b[1;36m${user}\x1b[0m@\x1b[1;36m${host}\x1b[0m`,
      '    `:+ssssssssssssssssss+:`           ',
      '  -+ssssssssssssssssssyyssss+-         ' + line('OS', 'WebOS 24.04 LTS x86_64'),
      '.ossssssssssssssssssdMMMNysssso.       ' + line('Kernel', '6.5.0-webos-generic'),
      '/ssssssssssshmmddmmhs+///+syssss/      ' + line('Uptime', '2 hours, 15 mins'),
      '+sssssssssyms+/::/+dmy+++++oyssss+     ' + line('Packages', '50 (webos-pkg)'),
      '+sssssssyso+++oo+++oyyso++++osssso     ' + line('Shell', 'webos-bash 5.2.15'),
      '/sssssssyso+++oNMMMMMMdo++++ossss/     ' + line('Resolution', '1920x1080'),
      '.ossssssssssssssssssdMMMNysssso.       ' + line('DE', 'WebOS Desktop'),
      '  -+sssssssssssssssssyyssss+-         ' + line('WM', 'WebOS Compositor'),
      '    `:+ssssssssssssssssss+:`           ' + line('Theme', 'Dark Lavender'),
      '        .-/+oossssoo+/-.               ' + line('Terminal', 'WebOS Terminal'),
      '                                        ' + line('CPU', 'WebOS vCPU (8) @ 3.2GHz'),
      '                                        ' + line('Memory', '4096MB / 16384MB'),
    ].join('\n');
  },

  whoami: () => 'user',

  date: () => new Date().toLocaleString('en-US', { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric', hour: '2-digit', minute: '2-digit', second: '2-digit' }),

  cal: () => {
    const now = new Date();
    const year = now.getFullYear();
    const month = now.getMonth();
    const monthName = now.toLocaleDateString('en-US', { month: 'long', year: 'numeric' });
    const firstDay = new Date(year, month, 1).getDay();
    const daysInMonth = new Date(year, month + 1, 0).getDate();
    const today = now.getDate();
    let result = `    ${monthName}\nSu Mo Tu We Th Fr Sa\n`;
    let line = '   '.repeat(firstDay);
    for (let d = 1; d <= daysInMonth; d++) {
      if (d === today) { line += `\x1b[1;35m${String(d).padStart(2)}\x1b[0m `; }
      else { line += `${String(d).padStart(2)} `; }
      if ((firstDay + d) % 7 === 0) { result += line.trimEnd() + '\n'; line = ''; }
    }
    if (line.trim()) result += line;
    return result;
  },

  df: () => {
    return `Filesystem     1K-blocks     Used Available Use% Mounted on
/dev/sda1       51200000  8400000  40240000  18% /
tmpfs            8388608    51200   8337408   1% /run
/dev/loop0      10000000  3000000   7000000  30% /usr
webos-vfs         102400    24576     77824  24% /home`;
  },

  free: () => {
    const total = 16384;
    const used = 4096;
    const free = total - used;
    return `              total        used        free      shared  buff/cache   available
Mem:        ${total}MB       ${used}MB       ${free}MB        256MB       2048MB      ${free + 2048}MB
Swap:        4096MB          0MB       4096MB`;
  },

  ps: () => {
    return `  PID TTY          TIME CMD
    1 ?        00:00:01 init
  234 ?        00:00:02 systemd
  456 ?        00:00:00 display-server
  567 ?        00:00:05 webos-compositor
  678 ?        00:00:01 webos-panel
  789 pts/0    00:00:00 bash
  890 pts/0    00:00:00 ps`;
  },

  top: () => `top - ${new Date().toLocaleTimeString()} up 2:15, 1 user, load average: 0.42, 0.38, 0.35
Tasks: 45 total,   1 running,  44 sleeping,   0 stopped,   0 zombie
%Cpu(s):  8.2 us,  2.1 sy,  0.0 ni, 88.7 id,  0.5 wa,  0.0 hi,  0.5 si,  0.0 st
MiB Mem :  16384.0 total,  12288.0 free,   4096.0 used,   2048.0 buff/cache
MiB Swap:   4096.0 total,   4096.0 free,      0.0 used.  11952.0 avail Mem

  PID USER      PR  NI    VIRT    RES    SHR S  %CPU %MEM     TIME+ COMMAND
  567 user      20   0  512.2m  89.4m  45.2m S   4.2  0.5   0:05.12 webos-compositor
  456 user      20   0  256.1m  42.1m  21.3m S   1.8  0.3   0:02.34 display-server
  234 root      20   0  128.5m  12.4m   8.1m S   0.5  0.1   0:01.23 systemd
    1 root      20   0   16.2m   4.8m   3.2m S   0.0  0.0   0:00.45 init`,

  find: (args, ctx) => {
    const fs = useFileSystemStore.getState();
    const dirId = args[0] ? resolvePath(args[0], ctx.cwd) : ctx.cwd;
    const name = args.find((a, i) => i > 0 && !a.startsWith('-')) || '';
    if (!dirId) return '';
    const results: string[] = [];
    const search = (nodeId: string, prefix: string) => {
      const children = fs.getChildren(nodeId);
      for (const child of children) {
        const path = prefix + '/' + child.name;
        if (!name || child.name.includes(name)) results.push(path);
        if (child.type === 'directory') search(child.id, path);
      }
    };
    const startNode = fs.getNode(dirId);
    search(dirId, startNode ? fs.getPath(dirId) : '');
    return results.slice(0, 50).join('\n') || '';
  },

  grep: (args, ctx) => {
    if (!args[0]) return 'grep: missing pattern';
    const pattern = args[0];
    const fs = useFileSystemStore.getState();
    const files = fs.getChildren(ctx.cwd).filter((c) => c.type === 'file');
    const results: string[] = [];
    for (const file of files) {
      const content = file.content || '';
      const lines = content.split('\n');
      lines.forEach((line, idx) => {
        if (line.includes(pattern)) results.push(`${file.name}:${idx + 1}:${line}`);
      });
    }
    return results.join('\n') || '';
  },

  chmod: (args) => {
    if (args.length < 2) return 'chmod: missing operand';
    return '';
  },

  history: (_, ctx) => ctx.history.map((h, i) => `${String(i + 1).padStart(4)}  ${h}`).join('\n'),

  uname: (args) => {
    if (args.includes('-a')) return 'WebOS webos-desktop 6.5.0-webos-generic #1 SMP PREEMPT WebOS 24.04 LTS x86_64 GNU/Linux';
    return 'WebOS';
  },

  hostname: () => 'webos-desktop',

  uptime: () => ' 02:15:00 up 2:15,  1 user,  load average: 0.42, 0.38, 0.35',

  wc: (args, ctx) => {
    if (!args[0]) return 'wc: missing file operand';
    const fs = useFileSystemStore.getState();
    const fileId = resolvePath(args[0], ctx.cwd);
    if (!fileId) return '';
    const node = fs.getNode(fileId);
    if (!node || node.type !== 'file') return '';
    const content = node.content || '';
    const lines = content.split('\n').length;
    const words = content.split(/\s+/).filter(Boolean).length;
    const chars = content.length;
    return `${lines} ${words} ${chars} ${node.name}`;
  },

  head: (args, ctx) => {
    if (!args[0]) return 'head: missing file operand';
    const fs = useFileSystemStore.getState();
    const fileId = resolvePath(args[0], ctx.cwd);
    if (!fileId) return '';
    const node = fs.getNode(fileId);
    if (!node || node.type !== 'file') return '';
    return (node.content || '').split('\n').slice(0, 10).join('\n');
  },

  tail: (args, ctx) => {
    if (!args[0]) return 'tail: missing file operand';
    const fs = useFileSystemStore.getState();
    const fileId = resolvePath(args[0], ctx.cwd);
    if (!fileId) return '';
    const node = fs.getNode(fileId);
    if (!node || node.type !== 'file') return '';
    return (node.content || '').split('\n').slice(-10).join('\n');
  },

  tree: (args, ctx) => {
    const fs = useFileSystemStore.getState();
    const dirId = args[0] ? resolvePath(args[0], ctx.cwd) : ctx.cwd;
    if (!dirId) return '';
    const results: string[] = [];
    const walk = (nodeId: string, prefix: string) => {
      const children = fs.getChildren(nodeId);
      children.forEach((child, i) => {
        const isLast = i === children.length - 1;
        const connector = isLast ? '└── ' : '├── ';
        results.push(prefix + connector + child.name);
        if (child.type === 'directory') {
          walk(child.id, prefix + (isLast ? '    ' : '│   '));
        }
      });
    };
    const node = fs.getNode(dirId);
    if (node) results.push(node.name || '.');
    walk(dirId, '');
    return results.join('\n');
  },

  du: (args, ctx) => {
    const fs = useFileSystemStore.getState();
    const dirId = args[0] ? resolvePath(args[0], ctx.cwd) : ctx.cwd;
    if (!dirId) return '';
    const node = fs.getNode(dirId);
    if (!node) return '';
    const getTotal = (id: string): number => {
      let total = 0;
      const children = fs.getChildren(id);
      for (const child of children) {
        total += child.size || 0;
        if (child.type === 'directory') total += getTotal(child.id);
      }
      return total;
    };
    return `${formatSize(getTotal(dirId))}\t${fs.getPath(dirId)}`;
  },

  id: () => 'uid=1000(user) gid=1000(user) groups=1000(user),4(adm),24(cdrom),27(sudo),30(dip)',

  env: () => `SHELL=/bin/bash
USER=user
HOME=/home/user
PATH=/usr/local/bin:/usr/bin:/bin
LANG=en_US.UTF-8
TERM=xterm-256color
DISPLAY=:0
XDG_SESSION_TYPE=x11
XDG_CURRENT_DESKTOP=WebOS`,

  exit: () => '__EXIT__',
};

export function executeCommand(input: string, ctx: CommandContext): string {
  const trimmed = input.trim();
  if (!trimmed) return '';

  const parts = trimmed.split(/\s+/);
  const cmd = parts[0];
  const args = parts.slice(1);

  if (cmd === 'clear') return '__CLEAR__';
  if (cmd === 'exit') return '__EXIT__';

  const handler = commands[cmd];
  if (!handler) return `${cmd}: command not found`;

  try {
    return handler(args, ctx);
  } catch (e) {
    return `${cmd}: error: ${e}`;
  }
}

export function getCompletions(input: string): string[] {
  const cmds = Object.keys(commands);
  if (!input.includes(' ')) {
    return cmds.filter((c) => c.startsWith(input));
  }
  return [];
}
