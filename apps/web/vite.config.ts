import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath, URL } from "node:url";
import { defineConfig } from "vite";

export default defineConfig({
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  plugins: [react(), tailwindcss()],
  build: {
    rollupOptions: {
      // The replay player is its own page, so rrweb never loads in the main bundle.
      input: {
        main: fileURLToPath(new URL("./index.html", import.meta.url)),
        player: fileURLToPath(new URL("./replay-player.html", import.meta.url)),
      },
    },
  },
  server: {
    proxy: {
      "/api": process.env.VITE_API_PROXY ?? "http://127.0.0.1:43124",
      "/data.js": process.env.VITE_API_PROXY ?? "http://127.0.0.1:43124",
      "/go": process.env.VITE_API_PROXY ?? "http://127.0.0.1:43124",
    },
  },
});
