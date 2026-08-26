import { useState, useEffect } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { Search, X } from 'lucide-react';
import { useAppRegistryStore } from '@/stores/useAppRegistryStore';
import { useWindowStore } from '@/stores/useWindowStore';
import AppIcon from '@/components/AppIcon';

interface ApplicationMenuProps {
  isOpen: boolean;
  onClose: () => void;
}

const categoryOrder = ['System', 'Utilities', 'Internet', 'Multimedia'];

export default function ApplicationMenu({ isOpen, onClose }: ApplicationMenuProps) {
  const apps = useAppRegistryStore((s) => s.apps);
  const openWindow = useWindowStore((s) => s.openWindow);
  const [search, setSearch] = useState('');
  const [activeCategory, setActiveCategory] = useState('All');

  useEffect(() => {
    if (!isOpen) { setSearch(''); setActiveCategory('All'); }
  }, [isOpen]);

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && isOpen) onClose();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [isOpen, onClose]);

  const categories = useAppRegistryStore.getState().getAllCategories();
  const sortedCategories = categoryOrder.filter((c) => categories.includes(c)).concat(categories.filter((c) => !categoryOrder.includes(c)));

  const allApps = Object.values(apps);

  const filteredApps = allApps.filter((app) => {
    const matchesSearch = !search || app.name.toLowerCase().includes(search.toLowerCase()) || app.description.toLowerCase().includes(search.toLowerCase());
    const matchesCategory = activeCategory === 'All' || app.category === activeCategory;
    return matchesSearch && matchesCategory;
  });

  const handleOpenApp = (appId: string) => {
    const app = apps[appId];
    if (!app) return;
    openWindow(appId, app.name, { width: app.defaultWidth, height: app.defaultHeight });
    onClose();
  };

  return (
    <AnimatePresence>
      {isOpen && (
        <motion.div
          initial={{ opacity: 0, y: 20 }}
          animate={{ opacity: 1, y: 0 }}
          exit={{ opacity: 0, y: 20 }}
          transition={{ duration: 0.2, ease: [0.4, 0, 0.2, 1] as [number, number, number, number] }}
          className="fixed inset-x-0 bottom-12 top-9 z-[45] flex flex-col items-center p-6 overflow-y-auto"
          style={{ background: 'rgba(30,30,30,0.98)', backdropFilter: 'blur(20px)' }}
          onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
        >
          {/* Search */}
          <div className="w-full max-w-xl mb-6 relative">
            <Search size={18} className="absolute left-3 top-1/2 -translate-y-1/2 text-white/50" />
            <input
              type="text"
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="Type to search..."
              autoFocus
              className="w-full h-10 pl-10 pr-10 rounded-lg text-sm text-white outline-none placeholder:text-white/40"
              style={{ background: 'rgba(255,255,255,0.08)', border: '1px solid rgba(255,255,255,0.1)' }}
            />
            {search && (
              <button onClick={() => setSearch('')} className="absolute right-3 top-1/2 -translate-y-1/2">
                <X size={16} className="text-white/50" />
              </button>
            )}
          </div>

          {/* Category tabs */}
          <div className="flex flex-wrap justify-center gap-2 mb-6">
            {(['All'] as string[]).concat(sortedCategories).map((cat) => (
              <button
                key={cat}
                onClick={() => setActiveCategory(cat)}
                className={`px-3 py-1 rounded-full text-xs transition-colors ${
                  activeCategory === cat ? 'text-white' : 'text-white/60 hover:bg-white/10'
                }`}
                style={activeCategory === cat ? { background: 'var(--accent-silver)' } : {}}
              >
                {cat}
              </button>
            ))}
          </div>

          {/* App grid */}
          <div className="w-full max-w-5xl">
            <div className="mb-6">
              <h3 className="text-xs font-bold uppercase tracking-widest text-white/50 mb-3 px-2">
                {activeCategory === 'All' ? 'Applications' : activeCategory}
              </h3>
              <div className="grid grid-cols-3 sm:grid-cols-4 md:grid-cols-6 lg:grid-cols-8 gap-2">
                {filteredApps.map((app, index) => (
                  <motion.button
                    key={app.id}
                    initial={{ opacity: 0, scale: 0.9 }}
                    animate={{ opacity: 1, scale: 1 }}
                    transition={{ delay: index * 0.025 }}
                    onClick={() => handleOpenApp(app.id)}
                    className="flex flex-col items-center gap-1.5 p-3 rounded-xl hover:bg-white/10 hover:scale-105 transition-all group"
                  >
                    <div className="w-14 h-14 flex items-center justify-center">
                      <AppIcon icon={app.icon} label={app.name} size={48} />
                    </div>
                    <span className="text-[11px] text-white text-center leading-tight max-w-full truncate">{app.name}</span>
                  </motion.button>
                ))}
              </div>
              {filteredApps.length === 0 && (
                <p className="py-12 text-center text-sm text-white/40">No applications found.</p>
              )}
            </div>
          </div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
