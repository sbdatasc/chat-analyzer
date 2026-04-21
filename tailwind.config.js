/** @type {import('tailwindcss').Config} */
module.exports = {
  content: ['./index.html', './src/**/*.{ts,tsx,js,jsx}'],
  theme: {
    extend: {
      colors: {
        surface: 'var(--surface)',
        'surface-elev': 'var(--surface-elev)',
        text: 'var(--text)',
        'text-muted': 'var(--text-muted)',
        'stone-1': 'var(--stone-1)',
        'stone-2': 'var(--stone-2)',
        'stone-3': 'var(--stone-3)',
        accent: 'var(--accent)',
        'accent-soft': 'var(--accent-soft)',
        'flag-amber': 'var(--flag-amber)',
        'flag-red': 'var(--flag-red)',
        'flag-green': 'var(--flag-green)',
      },
      fontFamily: {
        sans: ['"IBM Plex Sans"', 'system-ui', 'sans-serif'],
        mono: ['"IBM Plex Mono"', 'ui-monospace', 'monospace'],
      },
      borderRadius: {
        DEFAULT: '6px',
        lg: '10px',
      },
      transitionDuration: {
        DEFAULT: '120ms',
      },
      transitionTimingFunction: {
        DEFAULT: 'cubic-bezier(0.2, 0, 0, 1)',
      },
    },
  },
  plugins: [],
};
