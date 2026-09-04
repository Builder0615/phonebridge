import path from "node:path"
import { defineConfig } from "vitest/config"
import react from "@vitejs/plugin-react"
import tailwindcss from "@tailwindcss/vite"

// 快投屏：Vite 仅作前端构建；系统能力全部经由 Tauri commands/events 提供。
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "127.0.0.1",
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    // Tauri 使用系统 WebView（WebView2 / WKWebView），按 WebView 能力设定 target。
    target: ["es2021", "chrome105", "safari13"],
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
    rollupOptions: {
      // 主窗口和模拟器是两个独立入口；模拟器入口不包含 ControlPanel。
      input: {
        main: path.resolve(__dirname, "index.html"),
        simulator: path.resolve(__dirname, "simulator.html"),
      },
    },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
})
