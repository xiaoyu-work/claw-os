import { create } from 'zustand';

export interface AppDefinition {
  id: string;
  name: string;
  description: string;
  category: string;
  icon: string;
  component: string;
  defaultWidth: number;
  defaultHeight: number;
  minWidth: number;
  minHeight: number;
  singleton: boolean;
}

export interface AppRegistryStore {
  apps: Record<string, AppDefinition>;
  registerApp: (app: AppDefinition) => void;
  getApp: (id: string) => AppDefinition | undefined;
  getAppsByCategory: (category: string) => AppDefinition[];
  getAllCategories: () => string[];
}

const defaultApps: Record<string, AppDefinition> = {
  // Core System Apps
  terminal: {
    id: 'terminal', name: 'Terminal', description: 'Command-line terminal emulator',
    category: 'System', icon: 'Terminal', component: 'Terminal',
    defaultWidth: 800, defaultHeight: 500, minWidth: 400, minHeight: 200, singleton: false,
  },
  filemanager: {
    id: 'filemanager', name: 'File Manager', description: 'Browse and manage files',
    category: 'System', icon: 'FolderOpen', component: 'FileManager',
    defaultWidth: 900, defaultHeight: 600, minWidth: 500, minHeight: 300, singleton: false,
  },
  settings: {
    id: 'settings', name: 'Settings', description: 'System settings and preferences',
    category: 'System', icon: 'Settings', component: 'Settings',
    defaultWidth: 800, defaultHeight: 550, minWidth: 600, minHeight: 400, singleton: true,
  },
  taskmanager: {
    id: 'taskmanager', name: 'Task Manager', description: 'Monitor system processes and performance',
    category: 'System', icon: 'Activity', component: 'TaskManager',
    defaultWidth: 700, defaultHeight: 500, minWidth: 500, minHeight: 300, singleton: true,
  },
  calculator: {
    id: 'calculator', name: 'Calculator', description: 'Standard and scientific calculator',
    category: 'Accessories', icon: 'Calculator', component: 'Calculator',
    defaultWidth: 360, defaultHeight: 520, minWidth: 300, minHeight: 400, singleton: true,
  },
  texteditor: {
    id: 'texteditor', name: 'Text Editor', description: 'Edit text files',
    category: 'Accessories', icon: 'FileText', component: 'TextEditor',
    defaultWidth: 800, defaultHeight: 600, minWidth: 400, minHeight: 300, singleton: false,
  },
  calendar: {
    id: 'calendar', name: 'Calendar', description: 'View calendar and manage events',
    category: 'Accessories', icon: 'Calendar', component: 'Calendar',
    defaultWidth: 800, defaultHeight: 550, minWidth: 500, minHeight: 350, singleton: true,
  },
  clock: {
    id: 'clock', name: 'Clock', description: 'World clock with multiple timezones',
    category: 'Accessories', icon: 'Clock', component: 'Clock',
    defaultWidth: 500, defaultHeight: 400, minWidth: 400, minHeight: 300, singleton: true,
  },

  // Development
  codeeditor: {
    id: 'codeeditor', name: 'Code Editor', description: 'Advanced code editor with syntax highlighting',
    category: 'Development', icon: 'Code2', component: 'CodeEditor',
    defaultWidth: 1000, defaultHeight: 700, minWidth: 500, minHeight: 400, singleton: false,
  },
  gitclient: {
    id: 'gitclient', name: 'Git Client', description: 'Version control management',
    category: 'Development', icon: 'GitBranch', component: 'GitClient',
    defaultWidth: 900, defaultHeight: 600, minWidth: 500, minHeight: 400, singleton: true,
  },
  apiclient: {
    id: 'apiclient', name: 'API Client', description: 'Test HTTP APIs',
    category: 'Development', icon: 'Globe', component: 'ApiClient',
    defaultWidth: 800, defaultHeight: 600, minWidth: 500, minHeight: 400, singleton: false,
  },
  database: {
    id: 'database', name: 'Database Explorer', description: 'Browse SQLite databases',
    category: 'Development', icon: 'Database', component: 'Database',
    defaultWidth: 800, defaultHeight: 600, minWidth: 500, minHeight: 400, singleton: false,
  },
  regexbuddy: {
    id: 'regexbuddy', name: 'Regex Buddy', description: 'Test and build regular expressions',
    category: 'Development', icon: 'Search', component: 'RegexBuddy',
    defaultWidth: 700, defaultHeight: 450, minWidth: 500, minHeight: 300, singleton: true,
  },
  jsonviewer: {
    id: 'jsonviewer', name: 'JSON Viewer', description: 'Format and visualize JSON data',
    category: 'Development', icon: 'Braces', component: 'JsonViewer',
    defaultWidth: 700, defaultHeight: 500, minWidth: 400, minHeight: 300, singleton: false,
  },
  markdownviewer: {
    id: 'markdownviewer', name: 'Markdown Viewer', description: 'Preview rendered markdown',
    category: 'Development', icon: 'BookOpen', component: 'MarkdownViewer',
    defaultWidth: 900, defaultHeight: 650, minWidth: 500, minHeight: 400, singleton: false,
  },
  colorpicker: {
    id: 'colorpicker', name: 'Color Picker', description: 'Pick and convert colors',
    category: 'Development', icon: 'Palette', component: 'ColorPicker',
    defaultWidth: 450, defaultHeight: 400, minWidth: 350, minHeight: 300, singleton: true,
  },
  diffviewer: {
    id: 'diffviewer', name: 'Diff Viewer', description: 'Compare text differences',
    category: 'Development', icon: 'SplitSquareHorizontal', component: 'DiffViewer',
    defaultWidth: 900, defaultHeight: 600, minWidth: 500, minHeight: 400, singleton: false,
  },

  // Internet
  browser: {
    id: 'browser', name: 'Web Browser', description: 'Browse the web',
    category: 'Internet', icon: 'Globe', component: 'Browser',
    defaultWidth: 1100, defaultHeight: 700, minWidth: 600, minHeight: 400, singleton: false,
  },
  email: {
    id: 'email', name: 'Email Client', description: 'Send and receive emails',
    category: 'Internet', icon: 'Mail', component: 'Email',
    defaultWidth: 1000, defaultHeight: 650, minWidth: 600, minHeight: 400, singleton: false,
  },
  chat: {
    id: 'chat', name: 'Chat', description: 'Instant messaging',
    category: 'Internet', icon: 'MessageSquare', component: 'Chat',
    defaultWidth: 500, defaultHeight: 600, minWidth: 350, minHeight: 400, singleton: false,
  },
  weather: {
    id: 'weather', name: 'Weather', description: 'Weather forecasts',
    category: 'Internet', icon: 'CloudSun', component: 'Weather',
    defaultWidth: 450, defaultHeight: 550, minWidth: 350, minHeight: 400, singleton: true,
  },
  maps: {
    id: 'maps', name: 'Maps', description: 'Interactive maps and navigation',
    category: 'Internet', icon: 'Map', component: 'Maps',
    defaultWidth: 900, defaultHeight: 650, minWidth: 500, minHeight: 400, singleton: true,
  },
  news: {
    id: 'news', name: 'News Reader', description: 'Read the latest news',
    category: 'Internet', icon: 'Newspaper', component: 'NewsReader',
    defaultWidth: 800, defaultHeight: 650, minWidth: 500, minHeight: 400, singleton: false,
  },

  // Office
  writer: {
    id: 'writer', name: 'Writer', description: 'Word processing',
    category: 'Office', icon: 'PenTool', component: 'Writer',
    defaultWidth: 900, defaultHeight: 700, minWidth: 500, minHeight: 400, singleton: false,
  },
  spreadsheet: {
    id: 'spreadsheet', name: 'Spreadsheet', description: 'Spreadsheet editor',
    category: 'Office', icon: 'Table', component: 'Spreadsheet',
    defaultWidth: 1000, defaultHeight: 650, minWidth: 500, minHeight: 400, singleton: false,
  },
  presentation: {
    id: 'presentation', name: 'Presentation', description: 'Create slideshows',
    category: 'Office', icon: 'Presentation', component: 'Presentation',
    defaultWidth: 1000, defaultHeight: 650, minWidth: 500, minHeight: 400, singleton: false,
  },
  pdfviewer: {
    id: 'pdfviewer', name: 'PDF Viewer', description: 'View PDF documents',
    category: 'Office', icon: 'File', component: 'PdfViewer',
    defaultWidth: 800, defaultHeight: 650, minWidth: 500, minHeight: 400, singleton: false,
  },
  notepad: {
    id: 'notepad', name: 'Notepad', description: 'Quick notes',
    category: 'Office', icon: 'StickyNote', component: 'Notepad',
    defaultWidth: 500, defaultHeight: 500, minWidth: 300, minHeight: 300, singleton: false,
  },

  // Multimedia
  musicplayer: {
    id: 'musicplayer', name: 'Music Player', description: 'Play music files',
    category: 'Multimedia', icon: 'Music', component: 'MusicPlayer',
    defaultWidth: 450, defaultHeight: 650, minWidth: 350, minHeight: 500, singleton: true,
  },
  videoplayer: {
    id: 'videoplayer', name: 'Video Player', description: 'Play video files',
    category: 'Multimedia', icon: 'PlayCircle', component: 'VideoPlayer',
    defaultWidth: 800, defaultHeight: 550, minWidth: 500, minHeight: 350, singleton: false,
  },
  imageviewer: {
    id: 'imageviewer', name: 'Image Viewer', description: 'View and edit images',
    category: 'Multimedia', icon: 'Image', component: 'ImageViewer',
    defaultWidth: 800, defaultHeight: 600, minWidth: 400, minHeight: 300, singleton: false,
  },
  camera: {
    id: 'camera', name: 'Camera', description: 'Take photos with webcam',
    category: 'Multimedia', icon: 'Camera', component: 'Camera',
    defaultWidth: 640, defaultHeight: 500, minWidth: 400, minHeight: 350, singleton: true,
  },
  voice: {
    id: 'voice', name: 'Voice Recorder', description: 'Record audio',
    category: 'Multimedia', icon: 'Mic', component: 'VoiceRecorder',
    defaultWidth: 400, defaultHeight: 300, minWidth: 300, minHeight: 200, singleton: true,
  },

  // Graphics
  paint: {
    id: 'paint', name: 'Paint', description: 'Paint and draw',
    category: 'Graphics', icon: 'Paintbrush', component: 'Paint',
    defaultWidth: 900, defaultHeight: 700, minWidth: 500, minHeight: 400, singleton: false,
  },
  imageeditor: {
    id: 'imageeditor', name: 'Image Editor', description: 'Edit photos',
    category: 'Graphics', icon: 'ImagePlus', component: 'ImageEditor',
    defaultWidth: 1000, defaultHeight: 700, minWidth: 500, minHeight: 400, singleton: false,
  },
  svgviewer: {
    id: 'svgviewer', name: 'SVG Viewer', description: 'View and edit SVG',
    category: 'Graphics', icon: 'PenTool', component: 'SvgViewer',
    defaultWidth: 800, defaultHeight: 650, minWidth: 500, minHeight: 400, singleton: false,
  },
  iconmaker: {
    id: 'iconmaker', name: 'Icon Maker', description: 'Create app icons',
    category: 'Graphics', icon: 'Shapes', component: 'IconMaker',
    defaultWidth: 600, defaultHeight: 550, minWidth: 400, minHeight: 350, singleton: false,
  },

  // System Utilities
  screenshot: {
    id: 'screenshot', name: 'Screenshot', description: 'Take screenshots',
    category: 'System', icon: 'Camera', component: 'Screenshot',
    defaultWidth: 500, defaultHeight: 400, minWidth: 350, minHeight: 300, singleton: true,
  },
  systeminfo: {
    id: 'systeminfo', name: 'System Info', description: 'View system information',
    category: 'System', icon: 'Monitor', component: 'SystemInfo',
    defaultWidth: 600, defaultHeight: 500, minWidth: 400, minHeight: 350, singleton: true,
  },
  diskusage: {
    id: 'diskusage', name: 'Disk Usage', description: 'Analyze disk space',
    category: 'System', icon: 'HardDrive', component: 'DiskUsage',
    defaultWidth: 700, defaultHeight: 500, minWidth: 400, minHeight: 300, singleton: true,
  },
  backup: {
    id: 'backup', name: 'Backup', description: 'Backup and restore files',
    category: 'System', icon: 'Archive', component: 'Backup',
    defaultWidth: 600, defaultHeight: 450, minWidth: 400, minHeight: 300, singleton: true,
  },

  filesearch: {
    id: 'filesearch', name: 'File Search', description: 'Search files on the system',
    category: 'System', icon: 'Search', component: 'FileSearch',
    defaultWidth: 600, defaultHeight: 500, minWidth: 400, minHeight: 300, singleton: false,
  },
  network: {
    id: 'network', name: 'Network Tools', description: 'Network diagnostics',
    category: 'System', icon: 'Wifi', component: 'NetworkTools',
    defaultWidth: 700, defaultHeight: 500, minWidth: 450, minHeight: 350, singleton: true,
  },
  encrypter: {
    id: 'encrypter', name: 'Encrypter', description: 'Encrypt and decrypt text',
    category: 'System', icon: 'Lock', component: 'Encrypter',
    defaultWidth: 550, defaultHeight: 450, minWidth: 400, minHeight: 350, singleton: true,
  },
  archive: {
    id: 'archive', name: 'Archive Manager', description: 'Create and extract archives',
    category: 'System', icon: 'Package', component: 'Archive',
    defaultWidth: 600, defaultHeight: 450, minWidth: 400, minHeight: 300, singleton: false,
  },

  // Accessories
  help: {
    id: 'help', name: 'Help', description: 'WebOS help documentation',
    category: 'Accessories', icon: 'HelpCircle', component: 'Help',
    defaultWidth: 700, defaultHeight: 550, minWidth: 400, minHeight: 350, singleton: true,
  },
  trash: {
    id: 'trash', name: 'Trash', description: 'Deleted files',
    category: 'Accessories', icon: 'Trash2', component: 'Trash',
    defaultWidth: 700, defaultHeight: 500, minWidth: 400, minHeight: 300, singleton: false,
  },
  dictionary: {
    id: 'dictionary', name: 'Dictionary', description: 'Look up word definitions',
    category: 'Accessories', icon: 'BookOpen', component: 'Dictionary',
    defaultWidth: 500, defaultHeight: 450, minWidth: 350, minHeight: 300, singleton: false,
  },
  translator: {
    id: 'translator', name: 'Translator', description: 'Translate between languages',
    category: 'Accessories', icon: 'Languages', component: 'Translator',
    defaultWidth: 550, defaultHeight: 450, minWidth: 400, minHeight: 350, singleton: false,
  },

  stopwatch: {
    id: 'stopwatch', name: 'Stopwatch', description: 'Time events',
    category: 'Accessories', icon: 'Timer', component: 'Stopwatch',
    defaultWidth: 400, defaultHeight: 350, minWidth: 300, minHeight: 250, singleton: true,
  },
  // Game Launcher
  gamelauncher: {
    id: 'gamelauncher', name: 'Games', description: 'Browse and launch all games',
    category: 'System', icon: 'Gamepad2', component: 'GameLauncher',
    defaultWidth: 700, defaultHeight: 500, minWidth: 500, minHeight: 400, singleton: true,
  },
  chess: {
    id: 'chess', name: 'Chess', description: 'Chess with AI opponent',
    category: 'Games', icon: 'Crown', component: 'Chess',
    defaultWidth: 700, defaultHeight: 560, minWidth: 500, minHeight: 400, singleton: false,
  },
  minesweeper: {
    id: 'minesweeper', name: 'Minesweeper', description: 'Classic minesweeper puzzle',
    category: 'Games', icon: 'Bomb', component: 'Minesweeper',
    defaultWidth: 400, defaultHeight: 500, minWidth: 300, minHeight: 350, singleton: false,
  },
  tetris: {
    id: 'tetris', name: 'Tetris', description: 'Block stacking puzzle game',
    category: 'Games', icon: 'LayoutGrid', component: 'Tetris',
    defaultWidth: 450, defaultHeight: 600, minWidth: 350, minHeight: 450, singleton: false,
  },
  snake: {
    id: 'snake', name: 'Snake', description: 'Grow the snake and avoid walls',
    category: 'Games', icon: 'Snail', component: 'Snake',
    defaultWidth: 480, defaultHeight: 540, minWidth: 350, minHeight: 400, singleton: false,
  },
  solitaire: {
    id: 'solitaire', name: 'Solitaire', description: 'Klondike card solitaire',
    category: 'Games', icon: 'Clubs', component: 'Solitaire',
    defaultWidth: 700, defaultHeight: 560, minWidth: 500, minHeight: 400, singleton: false,
  },
  game2048: {
    id: 'game2048', name: '2048', description: 'Slide tiles to reach 2048',
    category: 'Games', icon: 'Grid3X3', component: 'Game2048',
    defaultWidth: 420, defaultHeight: 540, minWidth: 340, minHeight: 420, singleton: false,
  },
  tictactoe: {
    id: 'tictactoe', name: 'Tic-Tac-Toe', description: 'Classic tic-tac-toe with AI',
    category: 'Games', icon: 'XCircle', component: 'TicTacToe',
    defaultWidth: 360, defaultHeight: 440, minWidth: 300, minHeight: 350, singleton: false,
  },
  memorymatch: {
    id: 'memorymatch', name: 'Memory Match', description: 'Match pairs of cards',
    category: 'Games', icon: 'Brain', component: 'MemoryMatch',
    defaultWidth: 380, defaultHeight: 480, minWidth: 300, minHeight: 380, singleton: false,
  },
  sudoku: {
    id: 'sudoku', name: 'Sudoku', description: 'Number puzzle with solver',
    category: 'Games', icon: 'Grid2X2', component: 'Sudoku',
    defaultWidth: 480, defaultHeight: 540, minWidth: 380, minHeight: 420, singleton: false,
  },
  pong: {
    id: 'pong', name: 'Pong', description: 'Classic paddle ball game',
    category: 'Games', icon: 'CircleDot', component: 'Pong',
    defaultWidth: 640, defaultHeight: 480, minWidth: 480, minHeight: 360, singleton: false,
  },

  // Utility Accessories
  password: {
    id: 'password', name: 'Password Generator', description: 'Generate secure passwords',
    category: 'Accessories', icon: 'KeyRound', component: 'PasswordGenerator',
    defaultWidth: 400, defaultHeight: 560, minWidth: 320, minHeight: 420, singleton: true,
  },
  qrcode: {
    id: 'qrcode', name: 'QR Code Generator', description: 'Generate QR codes from text',
    category: 'Accessories', icon: 'QrCode', component: 'QRCodeGenerator',
    defaultWidth: 460, defaultHeight: 560, minWidth: 360, minHeight: 420, singleton: false,
  },
  converter: {
    id: 'converter', name: 'Unit Converter', description: 'Convert between units',
    category: 'Accessories', icon: 'ArrowLeftRight', component: 'UnitConverter',
    defaultWidth: 440, defaultHeight: 520, minWidth: 340, minHeight: 400, singleton: true,
  },
  stickynotes: {
    id: 'stickynotes', name: 'Sticky Notes', description: 'Create and manage notes',
    category: 'Accessories', icon: 'StickyNote', component: 'StickyNotes',
    defaultWidth: 500, defaultHeight: 440, minWidth: 380, minHeight: 320, singleton: false,
  },
  fonts: {
    id: 'fonts', name: 'Font Viewer', description: 'Preview and compare fonts',
    category: 'Accessories', icon: 'Type', component: 'FontViewer',
    defaultWidth: 600, defaultHeight: 480, minWidth: 440, minHeight: 360, singleton: true,
  },
  archiver: {
    id: 'archiver', name: 'Archive Manager', description: 'Browse and manage archives',
    category: 'Accessories', icon: 'Archive', component: 'ArchiveManager',
    defaultWidth: 580, defaultHeight: 460, minWidth: 400, minHeight: 340, singleton: false,
  },
};

export const useAppRegistryStore = create<AppRegistryStore>((set, get) => ({
  apps: defaultApps,

  registerApp: (app) =>
    set((state) => ({
      apps: { ...state.apps, [app.id]: app },
    })),

  getApp: (id) => get().apps[id],

  getAppsByCategory: (category) =>
    Object.values(get().apps).filter((app) => app.category === category),

  getAllCategories: () => {
    const cats = new Set(Object.values(get().apps).map((a) => a.category));
    return Array.from(cats).sort();
  },
}));
