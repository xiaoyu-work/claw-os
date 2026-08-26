import type { ComponentType } from 'react';
import * as Icons from 'lucide-react';
import type { LucideProps } from 'lucide-react';

interface AppIconProps {
  icon: string;
  label: string;
  size?: number;
  className?: string;
}

const lucideIcons = Icons as unknown as Record<string, ComponentType<LucideProps>>;

export default function AppIcon({ icon, label, size = 32, className = '' }: AppIconProps) {
  const LucideIcon = lucideIcons[icon];

  if (LucideIcon) {
    return <LucideIcon size={size} className={className} aria-hidden="true" />;
  }

  return (
    <img
      src={icon}
      alt=""
      aria-label={`${label} icon`}
      className={className}
      draggable={false}
      style={{ width: size, height: size, objectFit: 'contain' }}
    />
  );
}
