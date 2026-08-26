import { publicAsset } from '@/lib/publicAsset';

interface ClawOsAiIconProps {
  size?: number;
  className?: string;
}

export default function ClawOsAiIcon({ size = 18, className = '' }: ClawOsAiIconProps) {
  return (
    <img
      src={publicAsset('app-icons/agent.svg')}
      alt=""
      aria-hidden="true"
      draggable={false}
      className={`shrink-0 ${className}`}
      style={{ width: size, height: size, objectFit: 'contain' }}
    />
  );
}
