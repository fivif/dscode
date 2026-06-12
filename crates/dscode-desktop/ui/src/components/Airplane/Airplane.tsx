import { useState, useEffect, useCallback, useRef } from 'react';

interface Position {
  x: number;
  y: number;
}

interface TrailDot {
  x: number;
  y: number;
  id: number;
}

export default function Airplane() {
  const [pos, setPos] = useState<Position>({ x: 200, y: 200 });
  const [trail, setTrail] = useState<TrailDot[]>([]);
  const [keys, setKeys] = useState<Set<string>>(new Set());
  const [speed, setSpeed] = useState(3);
  const trailId = useRef(0);
  const animRef = useRef<number>(0);

  // Keyboard controls
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      setKeys((prev) => new Set(prev).add(e.key));
    };
    const handleKeyUp = (e: KeyboardEvent) => {
      setKeys((prev) => {
        const next = new Set(prev);
        next.delete(e.key);
        return next;
      });
    };
    window.addEventListener('keydown', handleKeyDown);
    window.addEventListener('keyup', handleKeyUp);
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
      window.removeEventListener('keyup', handleKeyUp);
    };
  }, []);

  // Game loop
  useEffect(() => {
    const loop = () => {
      setPos((prev) => {
        let dx = 0,
          dy = 0;
        if (keys.has('ArrowUp') || keys.has('w')) dy -= 1;
        if (keys.has('ArrowDown') || keys.has('s')) dy += 1;
        if (keys.has('ArrowLeft') || keys.has('a')) dx -= 1;
        if (keys.has('ArrowRight') || keys.has('d')) dx += 1;

        if (dx === 0 && dy === 0) return prev;

        // Normalize diagonal
        const len = Math.sqrt(dx * dx + dy * dy);
        dx = (dx / len) * speed;
        dy = (dy / len) * speed;

        const newX = Math.max(30, Math.min(window.innerWidth - 30, prev.x + dx));
        const newY = Math.max(30, Math.min(window.innerHeight - 30, prev.y + dy));

        return { x: newX, y: newY };
      });

      // Trail
      trailId.current += 1;
      setTrail((prev) => {
        const next = [
          ...prev,
          { x: pos.x, y: pos.y, id: trailId.current },
        ];
        return next.slice(-20);
      });

      animRef.current = requestAnimationFrame(loop);
    };
    animRef.current = requestAnimationFrame(loop);
    return () => cancelAnimationFrame(animRef.current);
  }, [keys, speed, pos.x, pos.y]);

  const rotateAngle = useCallback(() => {
    if (keys.has('ArrowUp') || keys.has('w')) return -15;
    if (keys.has('ArrowDown') || keys.has('s')) return 15;
    return 0;
  }, [keys]);

  const handleSpeedChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    setSpeed(Number(e.target.value));
  };

  return (
    <div className="fixed inset-0 z-50 pointer-events-none overflow-hidden bg-gradient-to-b from-sky-900/90 via-sky-700/80 to-sky-400/70">
      {/* Clouds */}
      <Cloud x="10%" y="15%" size={120} delay={0} />
      <Cloud x="70%" y="25%" size={90} delay={1.5} />
      <Cloud x="40%" y="55%" size={140} delay={0.8} />
      <Cloud x="80%" y="65%" size={100} delay={2.2} />
      <Cloud x="20%" y="75%" size={110} delay={0.3} />
      <Cloud x="55%" y="85%" size={80} delay={1.8} />

      {/* Trail */}
      {trail.map((dot, i) => (
        <div
          key={dot.id}
          className="absolute rounded-full pointer-events-none"
          style={{
            left: dot.x,
            top: dot.y,
            width: `${Math.max(2, (i / trail.length) * 8)}px`,
            height: `${Math.max(2, (i / trail.length) * 8)}px`,
            background: `rgba(255,255,255,${(i / trail.length) * 0.6})`,
            transform: 'translate(-50%, -50%)',
            transition: 'none',
          }}
        />
      ))}

      {/* Airplane */}
      <div
        className="absolute pointer-events-auto"
        style={{
          left: pos.x,
          top: pos.y,
          transform: `translate(-50%, -50%) rotate(${rotateAngle()}deg)`,
          transition: 'transform 0.08s linear',
        }}
      >
        <AirplaneSVG />
      </div>

      {/* HUD */}
      <div className="absolute top-4 left-4 pointer-events-auto bg-black/40 backdrop-blur rounded-xl p-4 text-white font-mono text-sm space-y-2">
        <div className="text-lg font-bold">✈️ 飞机控制</div>
        <div>位置: ({Math.round(pos.x)}, {Math.round(pos.y)})</div>
        <div className="flex items-center gap-2">
          <span>速度:</span>
          <input
            type="range"
            min={1}
            max={8}
            value={speed}
            onChange={handleSpeedChange}
            className="w-24 h-1 accent-sky-400"
          />
          <span>{speed}</span>
        </div>
        <div className="text-xs text-gray-300">
          ↑↓←→ 或 WASD 控制方向
        </div>
        <button
          onClick={() => {
            setPos({ x: window.innerWidth / 2, y: window.innerHeight / 2 });
            setTrail([]);
          }}
          className="px-3 py-1 bg-sky-500 hover:bg-sky-400 rounded text-xs transition-colors"
        >
          重置位置
        </button>
      </div>
    </div>
  );
}

/** A cute SVG airplane */
function AirplaneSVG() {
  return (
    <svg
      width="48"
      height="48"
      viewBox="0 0 48 48"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
    >
      {/* Body */}
      <ellipse cx="24" cy="24" rx="18" ry="5" fill="#e2e8f0" stroke="#94a3b8" strokeWidth="1" />
      {/* Nose */}
      <ellipse cx="40" cy="24" rx="5" ry="3.5" fill="#cbd5e1" stroke="#94a3b8" strokeWidth="1" />
      {/* Cockpit window */}
      <ellipse cx="34" cy="23" rx="3" ry="2.5" fill="#38bdf8" stroke="#0284c7" strokeWidth="0.8" />
      {/* Wings */}
      <polygon points="22,19 14,6 20,19" fill="#b0bec5" stroke="#78909c" strokeWidth="0.8" />
      <polygon points="22,29 14,42 20,29" fill="#b0bec5" stroke="#78909c" strokeWidth="0.8" />
      {/* Tail */}
      <polygon points="6,18 2,10 10,18" fill="#90a4ae" stroke="#607d8b" strokeWidth="0.8" />
      <polygon points="6,30 2,38 10,30" fill="#90a4ae" stroke="#607d8b" strokeWidth="0.8" />
      {/* Engine exhaust */}
      <ellipse cx="4" cy="24" rx="2" ry="1.5" fill="#fbbf24" />
    </svg>
  );
}

/** Animated floating cloud */
function Cloud({
  x,
  y,
  size,
  delay,
}: {
  x: string;
  y: string;
  size: number;
  delay: number;
}) {
  return (
    <div
      className="absolute opacity-40 pointer-events-none"
      style={{
        left: x,
        top: y,
        width: size,
        height: size * 0.6,
        animation: `floatCloud ${6 + delay * 2}s ease-in-out ${delay}s infinite`,
      }}
    >
      <svg viewBox="0 0 200 120" fill="white" xmlns="http://www.w3.org/2000/svg">
        <ellipse cx="70" cy="80" rx="60" ry="35" />
        <ellipse cx="110" cy="70" rx="55" ry="40" />
        <ellipse cx="145" cy="75" rx="45" ry="30" />
        <ellipse cx="100" cy="55" rx="50" ry="35" />
        <ellipse cx="60" cy="60" rx="40" ry="30" />
      </svg>
    </div>
  );
}
