import { defineConfig } from "vite";

// Safari 14 is the WKWebView floor on macOS 11; WebView2 is evergreen Chromium.
export default defineConfig({
  clearScreen: false,
  server: { port: 5174, strictPort: true },
  build: {
    target: ["es2021", "safari14"],
    outDir: "dist",
    assetsInlineLimit: 0,
    // The preload polyfill fetches modules; the page never needs fetch.
    modulePreload: { polyfill: false },
    sourcemap: false,
  },
});
