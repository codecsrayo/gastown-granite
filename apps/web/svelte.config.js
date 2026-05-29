import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    // adapter-static produces a fully static SPA; gt-api serves the build (hq-fe-cut.1).
    // fallback: SPA routing — every unknown path resolves to index.html.
    adapter: adapter({
      pages: 'build',
      assets: 'build',
      fallback: 'index.html',
      precompress: false,
      strict: true
    }),
    alias: {
      $lib: 'src/lib'
    }
  }
};

export default config;
