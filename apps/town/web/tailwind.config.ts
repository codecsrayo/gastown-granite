import type { Config } from 'tailwindcss';

// Theme via [data-theme] attribute on <html>; toggled by lib/stores/theme.svelte.ts.
// Dark = canonical (matches apps/town/docs/pagina.png); light = optional toggle.
export default {
  content: ['./src/**/*.{html,js,svelte,ts}'],
  darkMode: ['selector', '[data-theme="dark"]'],
  theme: {
    extend: {
      colors: {
        // Brand teal preserved across themes (matches wireframe + hi-fi mockup).
        accent: {
          DEFAULT: '#18a883',
          soft: '#1d5447'
        },
        warn: { DEFAULT: '#c47a2e', soft: '#3a2818' },
        bad: { DEFAULT: '#c2474a', soft: '#3a1a1c' }
      },
      fontFamily: {
        // Sketchbook stack mirrors the wireframe; replaced once we ship our own font tokens.
        sketch: ['Caveat', 'cursive'],
        body: ['Kalam', 'sans-serif'],
        mono: ['JetBrains Mono', 'ui-monospace', 'monospace']
      }
    }
  },
  plugins: []
} satisfies Config;
