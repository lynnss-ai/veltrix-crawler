import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";

const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },

  // 多页入口:主应用 + 录屏悬浮条独立轻量入口(悬浮窗只加载后者,秒开)
  build: {
    rollupOptions: {
      input: {
        main: path.resolve(__dirname, "index.html"),
        "recording-overlay": path.resolve(__dirname, "recording-overlay.html"),
      },
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    // 显式绑 IPv4:Node 17+ DNS 解析 localhost 优先返回 ::1,只绑 IPv6 时
    // WebView2 访问 devUrl(http://localhost:1420)走 IPv4 会被拒,窗口白屏
    host: host || "127.0.0.1",
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      // target/ 是 Rust 构建产物目录(十万级文件且频繁变动),chokidar 全量扫描会长时间
      // 阻塞事件循环,dev server 表现为「白屏、接口无响应」;dist/node_modules 同理排除
      ignored: [
        "**/src-tauri/**",
        "**/target/**",
        "**/dist/**",
        "**/node_modules/**",
        "**/.git/**",
      ],
    },
  },
}));
