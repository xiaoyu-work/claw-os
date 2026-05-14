import type { ImgHTMLAttributes } from "react";
import { cn } from "@/lib/utils";

type SymbolProps = {
  size?: number;
  className?: string;
} & Omit<ImgHTMLAttributes<HTMLImageElement>, "src" | "width" | "height" | "alt" | "className">;

// Brand symbol — the official Claw OS claw mark.
// Two artworks ship: the cobalt-on-transparent version for light
// backgrounds, and the inverse-on-black (white) version for dark
// theme. The theme provider toggles a 'dark' class on <html>, so we
// swap visibility with Tailwind dark: variants — no flash, no JS.
export function BrandSymbol({
  size = 28,
  className,
  ...rest
}: SymbolProps) {
  return (
    <span
      className={cn("inline-block shrink-0 relative", className)}
      style={{ width: size, height: size }}
    >
      <img
        src="/clawos-symbol.png"
        alt=""
        aria-hidden="true"
        width={size}
        height={size}
        className="absolute inset-0 size-full object-contain dark:hidden"
        {...rest}
      />
      <img
        src="/clawos-symbol-dark.png"
        alt=""
        aria-hidden="true"
        width={size}
        height={size}
        className="absolute inset-0 size-full object-contain hidden dark:block"
        {...rest}
      />
    </span>
  );
}

type WordmarkProps = {
  height?: number;
  className?: string;
} & Omit<ImgHTMLAttributes<HTMLImageElement>, "src" | "width" | "height" | "alt" | "className">;

// Wordmark "Claw OS" lockup (text-as-image). Use in place of
// <h1>Claw OS</h1> when on-brand typography matters.
export function BrandWordmark({
  height = 22,
  className,
  ...rest
}: WordmarkProps) {
  return (
    <span
      className={cn("inline-flex items-center shrink-0", className)}
      style={{ height }}
    >
      <img
        src="/clawos-wordmark.png"
        alt="Claw OS"
        height={height}
        className="h-full w-auto object-contain dark:hidden"
        {...rest}
      />
      <img
        src="/clawos-wordmark-dark.png"
        alt="Claw OS"
        height={height}
        className="h-full w-auto object-contain hidden dark:block"
        {...rest}
      />
    </span>
  );
}
