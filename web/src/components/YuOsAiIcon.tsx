import { publicAsset } from '@/lib/publicAsset';

interface YuOsAiIconProps {
  size?: number;
  className?: string;
}

export default function YuOsAiIcon({ size = 18, className = '' }: YuOsAiIconProps) {
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
