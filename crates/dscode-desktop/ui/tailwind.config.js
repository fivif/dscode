/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{js,ts,jsx,tsx}'],
  theme: {
    extend: {
      /**
       * THE token layer. Every surface, border and text colour in the app comes
       * from here — components must not reach for raw `gray-*` / `bg-white/[x]`,
       * because that is exactly how the previous palette drifted into ~5
       * near-identical greys and ~8 different hairline opacities.
       *
       * Values are the dark-theme half of DeepSeek Harness's `--dsw-static-*`
       * scale (MIT), mapped onto our own semantic names. Two things are worth
       * knowing when changing them:
       *
       *   · the neutral ramp is very slightly red-shifted (`nbu-*`, e.g.
       *     #232324) rather than the blue-shifted grey we used to carry — that
       *     is what keeps a near-black UI from reading as "computer blue".
       *   · the four text tiers are one continuous ramp (F9FAFB → CFD3D6 →
       *     ADB2B8 → 81858C), so `muted` is a real reading colour, not a
       *     barely-there grey. Anything that needs to be *read* at small sizes
       *     uses `muted` at minimum.
       */
      colors: {
        // ── Grounds (darkest → lightest) ──
        main: '#151517',      // app canvas behind everything
        sidebar: '#1B1B1C',   // sidebar plane, one step under the canvas
        card: '#232324',      // raised surface: tool cards, settings rows, menus
        input: '#1B1B1C',     // wells and fields that recede
        hover: '#2C2C2E',     // hover fill for rows and ghost buttons
        // The selected nav row. A translucent veil rather than a solid hex,
        // because it lands on the sidebar plane in one place and the settings
        // ground in another; one value reads correctly on both. dsh's
        // `interactive-bg-active` at 0.14 over `sidebar` resolves to #3B3B3C —
        // a hair *darker* than the accent-tinted fill it replaces, so selecting
        // a session is no louder than it was, only hueless.
        selected: 'rgba(255,255,255,0.14)',

        // ── Lines ──
        border: 'rgba(255,255,255,0.12)',   // the one hairline
        divider: 'rgba(255,255,255,0.06)',  // quieter separator, for dense lists

        // ── Text ──
        primary: '#F9FAFB',
        secondary: '#CFD3D6',
        muted: '#ADB2B8',
        faint: '#81858C',

        // ── Accent ──
        accent: '#679EFE',
        'accent-hover': '#B7C8FE',
        'accent-soft': '#34415B',  // accent-tinted fill: the chosen item inside a control (dropdown, segmented button, drop target) — not nav rows, which use `selected`

        // ── Semantic — never decorative, never the accent ──
        success: '#22C55E',
        warning: '#F59E0B',
        danger: '#F25A5A',
      },

      borderRadius: {
        // Continuous-ish curve scale. `card` matches the 14px used by menus and
        // panels; `bubble` is the large iMessage-style radius.
        //
        // `bubble` is 18px and its only consumer has moved off it: the user
        // message bubble now sets its own 22px (dsh's value) because it also
        // carries a tail, which needs per-corner control a token cannot give.
        // The token is deliberately left at 18px rather than chased to 22px —
        // it was sized for one caller, and changing a shared value to serve a
        // site that no longer reads it would silently move every future bubble.
        // If a second bubble appears, that is the moment to move it.
        card: '14px',
        bubble: '18px',
        control: '10px',
      },

      boxShadow: {
        /**
         * Elevation, in the DeepSeek Harness shape: the FIRST layer is a
         * spread-only 0.5px hairline stroke, not a `border`. That is what makes
         * a raised surface read as one crisp edge instead of a 1px border plus
         * a soft shadow stacked on top of it — so a surface using one of these
         * must carry `border: 0` (the `.panel` / `.menu` / `.modal` classes do
         * this for you).
         *
         * The glow layers are deliberately near-invisible (3-5% black). They
         * separate, they do not announce themselves.
         *
         * `hairline` is exported on its own for surfaces that need the edge
         * without a lift.
         */
        hairline: '0 0 0 0.5px rgba(255,255,255,0.20)',
        card: '0 0 0 0.5px rgba(255,255,255,0.20), 0 3px 8px 0 rgba(0,0,0,0.04), 0 0 20px 0 rgba(0,0,0,0.05)',
        pop: '0 0 0 0.5px rgba(255,255,255,0.20), 0 3px 8px 0 rgba(0,0,0,0.04), 0 0 20px 0 rgba(0,0,0,0.05)',
        modal: '0 0 0 0.5px rgba(255,255,255,0.20), 0 8px 28px -6px rgba(0,0,0,0.35), 0 0 32px 0 rgba(0,0,0,0.10)',
        // The same lift with the hairline stripped out, for a surface that
        // already draws its own coloured edge (a warning card, a state banner)
        // and would otherwise get a white seam laid over it.
        lift: '0 3px 8px 0 rgba(0,0,0,0.04), 0 0 20px 0 rgba(0,0,0,0.05)',
      },

      fontFamily: {
        // `-apple-system` never resolves inside WebView2 on Windows, so Segoe UI
        // Variable (Win11) leads. Keep the Apple faces after it for a future mac
        // build, and the generic stack as the floor.
        sans: [
          '"Segoe UI Variable Text"',
          '"Segoe UI"',
          '-apple-system',
          'BlinkMacSystemFont',
          '"SF Pro Text"',
          'Roboto',
          'system-ui',
          'sans-serif',
        ],
        mono: [
          '"Cascadia Code Variable"',
          '"Cascadia Mono"',
          'ui-monospace',
          '"SF Mono"',
          'Consolas',
          'monospace',
        ],
      },

      transitionTimingFunction: {
        // iOS uses a spring-ish ease-out for most state changes.
        ios: 'cubic-bezier(0.32, 0.72, 0, 1)',
      },
    },
  },
  plugins: [],
};
