import { useState, useEffect, useRef } from 'react';
import { motion, AnimatePresence } from 'framer-motion';
import { useSystemStore } from '@/stores/useSystemStore';

const bootMessages = [
  { text: 'WebOS Kernel v6.5.0-webos-generic (build@webos-builder) (gcc 13.2.0)', color: '#94A3B8' },
  { text: 'Command line: BOOT_IMAGE=/boot/vmlinuz-6.5.0 root=UUID=webos-rootfs ro quiet splash', color: '#94A3B8' },
  { text: 'KERNEL supported cpus:', color: '#94A3B8' },
  { text: '  Intel GenuineIntel', color: '#94A3B8' },
  { text: '  AMD AuthenticAMD', color: '#94A3B8' },
  { text: 'x86/fpu: Supporting XSAVE feature 0x001: \'x87 floating point registers\'', color: '#94A3B8' },
  { text: 'x86/fpu: xstate_offset[2]:  576, xstate_sizes[2]:  256', color: '#94A3B8' },
  { text: 'BIOS-provided physical RAM map:', color: '#64748B' },
  { text: 'BIOS-e820: [mem 0x0000000000000000-0x000000000009ffff] usable', color: '#64748B' },
  { text: 'NX (Execute Disable) protection: active', color: '#94A3B8' },
  { text: 'SMBIOS 3.3.0 present.', color: '#94A3B8' },
  { text: 'DMI: WebOS Virtual Machine, BIOS WebOS-EFI 1.0', color: '#94A3B8' },
  { text: 'Hypervisor detected: WebOS-VM', color: '#7D8B96' },
  { text: 'Zone ranges:', color: '#94A3B8' },
  { text: '  DMA      [mem 0x0000000000001000-0x0000000000ffffff]', color: '#64748B' },
  { text: '  DMA32    [mem 0x0000000001000000-0x00000000ffffffff]', color: '#64748B' },
  { text: '  Normal   [mem 0x0000000100000000-0x000000047fffffff]', color: '#64748B' },
  { text: 'WebOS Memory: 16384MB available (4096MB reserved for GPU)', color: '#22C55E' },
  { text: 'VFS: Disk quotas dquot_6.6.0', color: '#94A3B8' },
  { text: 'Block layer SCSI generic (bsg) driver version 0.4 loaded', color: '#94A3B8' },
  { text: 'WebOS-SSD: Detected NVMe controller (Samsung 990 PRO 2TB)', color: '#22C55E' },
  { text: 'Console: colour VGA+ 80x25', color: '#94A3B8' },
  { text: 'printk: console [tty0] enabled', color: '#94A3B8' },
  { text: 'WebOS Security Framework v2.1 initialized', color: '#22C55E' },
  { text: 'AppArmor: AppArmor initialized', color: '#94A3B8' },
  { text: 'Dentry cache hash table entries: 4194304 (order: 13, 33554432 bytes)', color: '#64748B' },
  { text: 'Inode-cache hash table entries: 2097152 (order: 12, 16777216 bytes)', color: '#64748B' },
  { text: 'Mount-cache hash table entries: 65536 (order: 7, 524288 bytes)', color: '#64748B' },
  { text: 'Initializing cgroup subsys cpu', color: '#94A3B8' },
  { text: 'Initializing cgroup subsys memory', color: '#94A3B8' },
  { text: 'WebOS Scheduler: CFS with latency nice support', color: '#94A3B8' },
  { text: 'rcu: Hierarchical SRCU implementation.', color: '#94A3B8' },
  { text: 'Calibrating delay loop (skipped), value calculated using timer frequency', color: '#94A3B8' },
  { text: 'CPU: Physical Processor ID: 0', color: '#94A3B8' },
  { text: 'ACPI: Core revision 20230331', color: '#94A3B8' },
  { text: 'clocksource: hpet: mask: 0xffffffff max_cycles: 0xffffffff', color: '#94A3B8' },
  { text: 'NetLabel: Initializing', color: '#94A3B8' },
  { text: 'WebOS Network Stack: IPv4, IPv6, WebSockets, WebRTC support', color: '#7D8B96' },
  { text: 'NET: Registered PF_INET protocol family', color: '#94A3B8' },
  { text: 'WebOS Display Manager: Initializing compositor...', color: '#7D8B96' },
  { text: 'WebOS Display Manager: 1920x1080 @ 144Hz detected', color: '#22C55E' },
  { text: 'WebOS Display Manager: GPU acceleration enabled (WebGL 2.0)', color: '#22C55E' },
  { text: 'i915 0000:00:02.0: enabling device (0000 -> 0003)', color: '#94A3B8' },
  { text: '[drm] Initialized i915 1.6.0 for 0000:00:02.0 on minor 0', color: '#7D8B96' },
  { text: 'fbcon: i915drmfb (fb0) is primary device', color: '#94A3B8' },
  { text: 'WebOS Audio Subsystem: PulseAudio bridge initialized', color: '#22C55E' },
  { text: 'WebOS Input Manager: Keyboard, mouse, touchpad detected', color: '#22C55E' },
  { text: 'WebOS App Runtime: Chromium 120 engine loaded', color: '#7D8B96' },
  { text: 'WebOS App Runtime: React 19 runtime initialized', color: '#7D8B96' },
  { text: 'WebOS File System: VirtualFS mounted at /', color: '#22C55E' },
  { text: 'WebOS File System: /home/user directory created', color: '#22C55E' },
  { text: 'WebOS Package Manager: 50 applications registered', color: '#7D8B96' },
  { text: 'usb 1-1: new high-speed USB device number 2 using xhci_hcd', color: '#94A3B8' },
  { text: 'input: WebOS Virtual Keyboard as /devices/virtual/input/input0', color: '#94A3B8' },
  { text: 'input: WebOS Virtual Mouse as /devices/virtual/input/input1', color: '#94A3B8' },
  { text: 'WebOS Power Manager: Battery at 87%, AC connected', color: '#22C55E' },
  { text: 'WebOS Update: System is up to date (build 2024.12.15)', color: '#22C55E' },
  { text: 'Freeing unused kernel image memory: 3072K', color: '#64748B' },
  { text: 'WebOS Kernel: Boot complete. Starting init...', color: '#F59E0B' },
  { text: 'Run /sbin/init as init process', color: '#94A3B8' },
  { text: '  with arguments:', color: '#94A3B8' },
  { text: '    /sbin/init', color: '#64748B' },
  { text: '    --splash', color: '#64748B' },
  { text: 'WebOS Init: Starting system services...', color: '#7D8B96' },
  { text: 'WebOS Init: Display server started', color: '#22C55E' },
  { text: 'WebOS Init: Session manager ready', color: '#22C55E' },
  { text: 'WebOS Init: Loading user profile...', color: '#22C55E' },
  { text: 'WebOS Init: Desktop environment starting...', color: '#22C55E' },
  { text: 'WebOS: Welcome. System ready.', color: '#E2E8F0' },
];

export default function BootSequence() {
  const bootPhase = useSystemStore((s) => s.bootPhase);
  const setBootPhase = useSystemStore((s) => s.setBootPhase);
  const [selectedEntry, setSelectedEntry] = useState(0);
  const [grubFadeOut, setGrubFadeOut] = useState(false);
  const [bootFadeOut, setBootFadeOut] = useState(false);
  const [visibleLines, setVisibleLines] = useState(0);
  const scrollRef = useRef<HTMLDivElement>(null);
  const bootEntries = [
    '* WebOS (6.5.0-webos-generic)',
    'WebOS (6.5.0-webos-generic, recovery mode)',
    'Memory test (memtest86+)',
    'Memory test (memtest86+, serial console)',
    'UEFI Firmware Settings',
  ];

  // GRUB countdown
  useEffect(() => {
    if (bootPhase !== 'grub') return;
    const keys = (e: KeyboardEvent) => {
      if (e.key === 'ArrowUp') { setSelectedEntry((p) => Math.max(0, p - 1)); e.preventDefault(); }
      if (e.key === 'ArrowDown') { setSelectedEntry((p) => Math.min(bootEntries.length - 1, p + 1)); e.preventDefault(); }
      if (e.key === 'Enter') { setGrubFadeOut(true); setTimeout(() => setBootPhase('booting'), 200); }
    };
    window.addEventListener('keydown', keys);
    const auto = setTimeout(() => { setGrubFadeOut(true); setTimeout(() => setBootPhase('booting'), 200); }, 3000);
    return () => { window.removeEventListener('keydown', keys); clearTimeout(auto); };
  }, [bootPhase]);

  // Boot message scrolling
  useEffect(() => {
    if (bootPhase !== 'booting') return;
    let idx = 0;
    const interval = setInterval(() => {
      idx++;
      setVisibleLines(idx);
      if (scrollRef.current) { scrollRef.current.scrollTop = scrollRef.current.scrollHeight; }
      if (idx >= bootMessages.length) {
        clearInterval(interval);
        setTimeout(() => { setBootFadeOut(true); setTimeout(() => setBootPhase('login'), 400); }, 600);
      }
    }, 80);
    const skip = (e: KeyboardEvent) => { if (e.key === 'Escape') { clearInterval(interval); setBootFadeOut(true); setTimeout(() => setBootPhase('login'), 200); } };
    window.addEventListener('keydown', skip);
    return () => { clearInterval(interval); window.removeEventListener('keydown', skip); };
  }, [bootPhase]);

  if (bootPhase === 'grub') {
    return (
      <div className={`fixed inset-0 z-[9999] flex flex-col items-center justify-center transition-opacity duration-200 ${grubFadeOut ? 'opacity-0' : 'opacity-100'}`}
        style={{ background: '#E8E8E8' }}>
        <div className="relative z-10 w-full max-w-xl">
          <div className="text-center mb-8">
            <div className="w-16 h-16 mx-auto mb-3 rounded-xl flex items-center justify-center" style={{ background: 'linear-gradient(135deg, #5A6670, #7D8B96)' }}>
              <span className="text-3xl font-bold text-white">W</span>
            </div>
            <h1 className="text-xl font-semibold text-[var(--text-primary)]">WebOS</h1>
            <p className="text-sm text-[var(--text-muted)]">Select an operating system to boot</p>
          </div>
          <div className="flex flex-col gap-0.5">
            {bootEntries.map((entry, i) => (
              <button
                key={i}
                onClick={() => { setSelectedEntry(i); setGrubFadeOut(true); setTimeout(() => setBootPhase('booting'), 200); }}
                className={`text-left px-5 py-3 text-sm transition-colors ${
                  selectedEntry === i ? 'text-[var(--text-primary)]' : 'text-[var(--text-secondary)]'
                }`}
                style={selectedEntry === i ? { background: '#E0E0E0', borderLeft: '3px solid var(--accent-silver)' } : { background: 'transparent', borderLeft: '3px solid transparent' }}
              >
                {entry}
              </button>
            ))}
          </div>
          <p className="mt-4 text-[11px] text-[var(--text-muted)] font-mono text-center">
            Use &uarr; and &darr; keys to select, Enter to boot
          </p>
        </div>
      </div>
    );
  }

  if (bootPhase === 'booting') {
    return (
      <div className={`fixed inset-0 z-[9999] transition-opacity duration-400 ${bootFadeOut ? 'opacity-0' : 'opacity-100'}`}
        style={{ background: '#E8E8E8' }}>
        <div ref={scrollRef} className="absolute inset-0 overflow-y-auto p-6 font-mono text-sm">
          {bootMessages.slice(0, visibleLines).map((msg, i) => (
            <div key={i} style={{ color: msg.color, lineHeight: 1.6 }}>
              <span className="text-[var(--text-muted)]">[{ (i * 0.004).toFixed(6) }]</span>{' '}
              {msg.text}
            </div>
          ))}
        </div>
        <div className="absolute bottom-4 right-4 text-[11px] text-[var(--text-muted)] font-mono">
          Press Esc to skip boot...
        </div>
      </div>
    );
  }

  return null;
}
