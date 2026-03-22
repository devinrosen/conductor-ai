import { useEffect, useState, useRef } from "react";

/**
 * Split-flap (Solari board) text display.
 *
 * Characters flip individually with a staggered delay when the text changes,
 * mimicking a mechanical departure board. Each character cycles through
 * intermediate values before landing on the target.
 */

const CHARS = " ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-:.·";
const FLIP_INTERVAL = 40; // ms per intermediate character
const STAGGER = 30; // ms delay between each character position

function charIndex(c: string): number {
  const idx = CHARS.indexOf(c.toUpperCase());
  return idx >= 0 ? idx : 0;
}

interface FlapCharProps {
  target: string;
  delay: number;
}

function FlapChar({ target, delay }: FlapCharProps) {
  const [display, setDisplay] = useState(target);
  const [flipping, setFlipping] = useState(false);
  const prevTarget = useRef(target);

  useEffect(() => {
    if (target === prevTarget.current) return;
    prevTarget.current = target;

    const targetIdx = charIndex(target);
    let currentIdx = charIndex(display);

    // If already correct, no flip needed
    if (currentIdx === targetIdx) return;

    const timeout = setTimeout(() => {
      setFlipping(true);
      const steps: string[] = [];

      // Cycle forward through the character set to reach target
      while (currentIdx !== targetIdx) {
        currentIdx = (currentIdx + 1) % CHARS.length;
        steps.push(CHARS[currentIdx]);
      }

      // Animate through steps
      let i = 0;
      const interval = setInterval(() => {
        setDisplay(steps[i]);
        i++;
        if (i >= steps.length) {
          clearInterval(interval);
          setFlipping(false);
        }
      }, FLIP_INTERVAL);

      return () => clearInterval(interval);
    }, delay);

    return () => clearTimeout(timeout);
  }, [target, delay, display]);

  return (
    <span
      className={`inline-block w-[0.65em] text-center font-mono transition-transform ${
        flipping ? "scale-y-95" : ""
      }`}
    >
      {display}
    </span>
  );
}

interface SplitFlapProps {
  text: string;
  /** Pad text to this length so the board has a fixed width. */
  length?: number;
  className?: string;
}

export function SplitFlap({ text, length, className = "" }: SplitFlapProps) {
  const padded = length ? text.toUpperCase().padEnd(length).slice(0, length) : text.toUpperCase();

  return (
    <span
      className={`inline-flex bg-gray-900 text-yellow-400 rounded px-1.5 py-0.5 font-mono text-xs tracking-widest ${className}`}
    >
      {padded.split("").map((char, i) => (
        <FlapChar key={i} target={char} delay={i * STAGGER} />
      ))}
    </span>
  );
}
