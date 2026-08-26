import { ArrowLeft, ArrowUp, RefreshCw, Plus, Trash2, Copy, Scissors, ClipboardPaste } from 'lucide-react';

interface FileToolbarProps {
  onBack: () => void;
  onUp: () => void;
  onRefresh: () => void;
  onNewFolder: () => void;
  onDelete: () => void;
  onCopy: () => void;
  onCut: () => void;
  onPaste: () => void;
  canGoBack: boolean;
  hasSelection: boolean;
  canPaste: boolean;
  viewMode: 'list' | 'grid';
  onViewModeChange: (mode: 'list' | 'grid') => void;
}

export default function FileToolbar(props: FileToolbarProps) {
  return null; // Inline toolbar in main component
}
