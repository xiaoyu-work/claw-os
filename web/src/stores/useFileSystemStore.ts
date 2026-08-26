import { create } from 'zustand';

export interface FSNode {
  id: string;
  name: string;
  type: 'file' | 'directory';
  parentId: string | null;
  content?: string;
  size?: number;
  createdAt: Date;
  modifiedAt: Date;
  permissions: string;
  owner: string;
  mimeType?: string;
}

export interface FileSystemStore {
  nodes: FSNode[];
  currentDirectory: string;
  clipboard: { nodeId: string; action: 'copy' | 'cut' } | null;

  getNode: (id: string) => FSNode | undefined;
  getChildren: (parentId: string) => FSNode[];
  getPath: (nodeId: string) => string;
  getNodeByPath: (path: string) => FSNode | undefined;
  createFile: (name: string, parentId: string, content?: string) => void;
  createDirectory: (name: string, parentId: string) => void;
  deleteNode: (id: string) => void;
  renameNode: (id: string, newName: string) => void;
  moveNode: (id: string, newParentId: string) => void;
  readFile: (id: string) => string | undefined;
  writeFile: (id: string, content: string) => void;
  setCurrentDirectory: (id: string) => void;
  copyToClipboard: (nodeId: string, action: 'copy' | 'cut') => void;
  clearClipboard: () => void;
  pasteFromClipboard: (parentId: string) => void;
}

function generateId() {
  return `fs-${Date.now()}-${Math.random().toString(36).substring(2, 9)}`;
}

const now = new Date();

const initialNodes: FSNode[] = [
  // Root
  { id: 'fs-root', name: '', type: 'directory', parentId: null, permissions: 'drwxr-xr-x', owner: 'root', size: 4096, createdAt: now, modifiedAt: now },
  { id: 'fs-bin', name: 'bin', type: 'directory', parentId: 'fs-root', permissions: 'drwxr-xr-x', owner: 'root', size: 4096, createdAt: now, modifiedAt: now },
  { id: 'fs-etc', name: 'etc', type: 'directory', parentId: 'fs-root', permissions: 'drwxr-xr-x', owner: 'root', size: 4096, createdAt: now, modifiedAt: now },
  { id: 'fs-home', name: 'home', type: 'directory', parentId: 'fs-root', permissions: 'drwxr-xr-x', owner: 'root', size: 4096, createdAt: now, modifiedAt: now },
  { id: 'fs-tmp', name: 'tmp', type: 'directory', parentId: 'fs-root', permissions: 'drwxrwxrwt', owner: 'root', size: 4096, createdAt: now, modifiedAt: now },
  { id: 'fs-usr', name: 'usr', type: 'directory', parentId: 'fs-root', permissions: 'drwxr-xr-x', owner: 'root', size: 4096, createdAt: now, modifiedAt: now },
  { id: 'fs-var', name: 'var', type: 'directory', parentId: 'fs-root', permissions: 'drwxr-xr-x', owner: 'root', size: 4096, createdAt: now, modifiedAt: now },
  // /home/user
  { id: 'fs-user', name: 'user', type: 'directory', parentId: 'fs-home', permissions: 'drwxr-xr-x', owner: 'user', size: 4096, createdAt: now, modifiedAt: now },
  { id: 'fs-user-desktop', name: 'Desktop', type: 'directory', parentId: 'fs-user', permissions: 'drwxr-xr-x', owner: 'user', size: 4096, createdAt: now, modifiedAt: now },
  { id: 'fs-user-doc', name: 'Documents', type: 'directory', parentId: 'fs-user', permissions: 'drwxr-xr-x', owner: 'user', size: 4096, createdAt: now, modifiedAt: now },
  { id: 'fs-user-dl', name: 'Downloads', type: 'directory', parentId: 'fs-user', permissions: 'drwxr-xr-x', owner: 'user', size: 4096, createdAt: now, modifiedAt: now },
  { id: 'fs-user-music', name: 'Music', type: 'directory', parentId: 'fs-user', permissions: 'drwxr-xr-x', owner: 'user', size: 4096, createdAt: now, modifiedAt: now },
  { id: 'fs-user-pics', name: 'Pictures', type: 'directory', parentId: 'fs-user', permissions: 'drwxr-xr-x', owner: 'user', size: 4096, createdAt: now, modifiedAt: now },
  { id: 'fs-user-vids', name: 'Videos', type: 'directory', parentId: 'fs-user', permissions: 'drwxr-xr-x', owner: 'user', size: 4096, createdAt: now, modifiedAt: now },
  // Sample files
  { id: 'fs-readme', name: 'README.txt', type: 'file', parentId: 'fs-user', content: 'Welcome to WebOS!\n\nThis is a fully functional web-based operating system.\nExplore the desktop, open applications, and enjoy the experience.\n\n- Double-click icons to open them\n- Right-click the desktop for more options\n- Use the Activities menu to find all apps\n', permissions: '-rw-r--r--', owner: 'user', size: 256, mimeType: 'text/plain', createdAt: now, modifiedAt: now },
  { id: 'fs-motd', name: 'motd', type: 'file', parentId: 'fs-etc', content: 'Welcome to WebOS 24.04 LTS!\n\n* Documentation: https://webos.dev/docs\n* Support: https://webos.dev/support\n\n0 packages can be updated.\n0 updates are security updates.\n', permissions: '-rw-r--r--', owner: 'root', size: 128, mimeType: 'text/plain', createdAt: now, modifiedAt: now },
  { id: 'fs-hosts', name: 'hosts', type: 'file', parentId: 'fs-etc', content: '127.0.0.1\tlocalhost\n127.0.0.1\twebos.local\n::1\t\tlocalhost ip6-localhost ip6-loopback\n', permissions: '-rw-r--r--', owner: 'root', size: 96, mimeType: 'text/plain', createdAt: now, modifiedAt: now },
  { id: 'fs-notes', name: 'notes.txt', type: 'file', parentId: 'fs-user-doc', content: 'Project Ideas:\n- Build a WebOS\n- Create awesome apps\n- Learn React\n\nShopping List:\n- Coffee\n- Pizza\n- More coffee\n', permissions: '-rw-r--r--', owner: 'user', size: 96, mimeType: 'text/plain', createdAt: now, modifiedAt: now },
  { id: 'fs-todo', name: 'todo.txt', type: 'file', parentId: 'fs-user-doc', content: '[x] Design the OS\n[x] Build the kernel\n[x] Create the desktop\n[ ] Add more apps\n[ ] World domination\n', permissions: '-rw-r--r--', owner: 'user', size: 90, mimeType: 'text/plain', createdAt: now, modifiedAt: now },
  { id: 'fs-song1', name: 'ambient.mp3', type: 'file', parentId: 'fs-user-music', content: '[MP3 Audio Data - Ambient Track]', permissions: '-rw-r--r--', owner: 'user', size: 4200000, mimeType: 'audio/mpeg', createdAt: now, modifiedAt: now },
  { id: 'fs-song2', name: 'lofi.mp3', type: 'file', parentId: 'fs-user-music', content: '[MP3 Audio Data - LoFi Track]', permissions: '-rw-r--r--', owner: 'user', size: 3800000, mimeType: 'audio/mpeg', createdAt: now, modifiedAt: now },
  { id: 'fs-photo1', name: 'vacation.jpg', type: 'file', parentId: 'fs-user-pics', content: '[JPEG Image Data]', permissions: '-rw-r--r--', owner: 'user', size: 2400000, mimeType: 'image/jpeg', createdAt: now, modifiedAt: now },
  { id: 'fs-photo2', name: 'screenshot.png', type: 'file', parentId: 'fs-user-pics', content: '[PNG Image Data]', permissions: '-rw-r--r--', owner: 'user', size: 1200000, mimeType: 'image/png', createdAt: now, modifiedAt: now },
  { id: 'fs-video1', name: 'demo.mp4', type: 'file', parentId: 'fs-user-vids', content: '[MP4 Video Data]', permissions: '-rw-r--r--', owner: 'user', size: 15000000, mimeType: 'video/mp4', createdAt: now, modifiedAt: now },
  { id: 'fs-download1', name: 'webos-update.zip', type: 'file', parentId: 'fs-user-dl', content: '[ZIP Archive]', permissions: '-rw-r--r--', owner: 'user', size: 50000000, mimeType: 'application/zip', createdAt: now, modifiedAt: now },
  { id: 'fs-download2', name: 'resume.pdf', type: 'file', parentId: 'fs-user-dl', content: '[PDF Document]', permissions: '-rw-r--r--', owner: 'user', size: 450000, mimeType: 'application/pdf', createdAt: now, modifiedAt: now },
  // Trash
  { id: 'fs-trash', name: '.Trash', type: 'directory', parentId: 'fs-user', permissions: 'drwxr-xr-x', owner: 'user', size: 4096, createdAt: now, modifiedAt: now },
];

export const useFileSystemStore = create<FileSystemStore>((set, get) => ({
  nodes: initialNodes,
  currentDirectory: 'fs-user',
  clipboard: null,

  getNode: (id) => get().nodes.find((n) => n.id === id),

  getChildren: (parentId) => get().nodes.filter((n) => n.parentId === parentId),

  getPath: (nodeId) => {
    const path: string[] = [];
    let current = get().nodes.find((n) => n.id === nodeId);
    while (current) {
      if (current.id === 'fs-root') {
        path.unshift('');
        break;
      }
      path.unshift(current.name);
      current = current.parentId ? get().nodes.find((n) => n.id === current!.parentId) : undefined;
    }
    return path.join('/') || '/';
  },

  getNodeByPath: (path) => {
    if (path === '/' || path === '') return get().nodes.find((n) => n.id === 'fs-root');
    const parts = path.split('/').filter(Boolean);
    let current = get().nodes.find((n) => n.id === 'fs-root');
    for (const part of parts) {
      const children = get().nodes.filter((n) => n.parentId === current?.id);
      current = children.find((n) => n.name === part);
      if (!current) return undefined;
    }
    return current;
  },

  createFile: (name, parentId, content = '') =>
    set((state) => ({
      nodes: [
        ...state.nodes,
        {
          id: generateId(),
          name,
          type: 'file',
          parentId,
          content,
          permissions: '-rw-r--r--',
          owner: 'user',
          size: content.length,
          mimeType: 'text/plain',
          createdAt: new Date(),
          modifiedAt: new Date(),
        },
      ],
    })),

  createDirectory: (name, parentId) =>
    set((state) => ({
      nodes: [
        ...state.nodes,
        {
          id: generateId(),
          name,
          type: 'directory',
          parentId,
          permissions: 'drwxr-xr-x',
          owner: 'user',
          size: 4096,
          createdAt: new Date(),
          modifiedAt: new Date(),
        },
      ],
    })),

  deleteNode: (id) => {
    const children = get().nodes.filter((n) => n.parentId === id);
    children.forEach((child) => get().deleteNode(child.id));
    set((state) => ({
      nodes: state.nodes.filter((n) => n.id !== id),
    }));
  },

  renameNode: (id, newName) =>
    set((state) => ({
      nodes: state.nodes.map((n) =>
        n.id === id ? { ...n, name: newName, modifiedAt: new Date() } : n
      ),
    })),

  moveNode: (id, newParentId) =>
    set((state) => ({
      nodes: state.nodes.map((n) =>
        n.id === id ? { ...n, parentId: newParentId, modifiedAt: new Date() } : n
      ),
    })),

  readFile: (id) => {
    const node = get().nodes.find((n) => n.id === id);
    return node?.content;
  },

  writeFile: (id, content) =>
    set((state) => ({
      nodes: state.nodes.map((n) =>
        n.id === id ? { ...n, content, size: content.length, modifiedAt: new Date() } : n
      ),
    })),

  setCurrentDirectory: (id) => set({ currentDirectory: id }),

  copyToClipboard: (nodeId, action) => set({ clipboard: { nodeId, action } }),
  clearClipboard: () => set({ clipboard: null }),

  pasteFromClipboard: (parentId) => {
    const { clipboard } = get();
    if (!clipboard) return;
    const node = get().nodes.find((n) => n.id === clipboard.nodeId);
    if (!node) return;
    if (clipboard.action === 'cut') {
      set((state) => ({
        nodes: state.nodes.map((n) =>
          n.id === clipboard.nodeId ? { ...n, parentId, modifiedAt: new Date() } : n
        ),
        clipboard: null,
      }));
    } else {
      const newNode: FSNode = {
        ...node,
        id: generateId(),
        name: node.type === 'directory' ? `${node.name}_copy` : (() => {
          const dotIdx = node.name.lastIndexOf('.');
          if (dotIdx === -1) return `${node.name}_copy`;
          return `${node.name.substring(0, dotIdx)}_copy${node.name.substring(dotIdx)}`;
        })(),
        parentId,
        createdAt: new Date(),
        modifiedAt: new Date(),
      };
      set((state) => ({ nodes: [...state.nodes, newNode] }));
    }
  },
}));
