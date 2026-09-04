/// <reference types="vite/client" />

interface Window {
  // Tauri 2 注入的引导信息，由 dsh/tauri 提供，浏览器预览环境下不存在。
  __TAURI_INTERNALS__?: unknown
}