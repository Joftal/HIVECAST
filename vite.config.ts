import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath, URL } from "node:url";

// Tauri 期望固定端口；开发时前端由 tauri dev 拉起。
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  clearScreen: false,
  server: { port: 1420, strictPort: true, watch: { ignored: ["**/src-tauri/**"] } },
  build: {
    target: "safari15",
    sourcemap: true,
    // hls.js 单库约 540 kB 且已按需懒加载，阈值留足单库余量，业务主包仍保持在 300 kB 以下。
    chunkSizeWarningLimit: 600,
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [
            { name: "hls", test: /node_modules[\\/]hls\.js[\\/]/, priority: 30 },
            { name: "react", test: /node_modules[\\/](react|react-dom|scheduler)[\\/]/, priority: 25 },
            { name: "motion", test: /node_modules[\\/](motion|motion-dom|motion-utils|framer-motion)[\\/]/, priority: 20 },
            { name: "radix", test: /node_modules[\\/]@radix-ui[\\/]/, priority: 15 },
          ],
        },
      },
    },
  },
});
