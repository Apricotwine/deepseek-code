import { useState, useEffect, useRef } from "react";

interface StreamingTextProps {
  text: string;
  speed?: number; // ms per chunk
  onComplete?: () => void;
}

export default function StreamingText({ text, speed = 8, onComplete }: StreamingTextProps) {
  const [displayed, setDisplayed] = useState("");
  const [done, setDone] = useState(false);
  const idxRef = useRef(0);
  const rafRef = useRef<number | null>(null);

  useEffect(() => {
    idxRef.current = 0;
    setDisplayed("");
    setDone(false);

    if (!text) return;

    const charsPerTick = 4;
    let lastTime = performance.now();
    let accumulator = 0;
    const interval = speed;

    const tick = (now: number) => {
      const delta = now - lastTime;
      lastTime = now;
      accumulator += delta;

      while (accumulator >= interval && idxRef.current < text.length) {
        accumulator -= interval;
        idxRef.current += charsPerTick;
        if (idxRef.current > text.length) idxRef.current = text.length;
      }

      setDisplayed(text.slice(0, idxRef.current));

      if (idxRef.current < text.length) {
        rafRef.current = requestAnimationFrame(tick);
      } else {
        setDone(true);
        onComplete?.();
      }
    };

    rafRef.current = requestAnimationFrame(tick);

    return () => {
      if (rafRef.current) cancelAnimationFrame(rafRef.current);
    };
  }, [text, speed]);

  return <span className={done ? "" : "streaming-cursor"}>{displayed}</span>;
}
