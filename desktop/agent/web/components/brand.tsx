import type { SVGProps } from "react";

export function BrandSymbol({
  size = 28,
  ...props
}: { size?: number } & Omit<SVGProps<SVGSVGElement>, "width" | "height">) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 256 256"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      {...props}
    >
      <g stroke="currentColor" strokeLinecap="round">
        <line x1="60" y1="20" x2="115" y2="218" strokeWidth="40" />
        <line x1="130" y1="74" x2="170" y2="218" strokeWidth="36" />
        <line x1="185" y1="128" x2="205" y2="218" strokeWidth="30" />
      </g>
      <circle cx="230" cy="220" r="18" fill="#005CFE" />
    </svg>
  );
}
