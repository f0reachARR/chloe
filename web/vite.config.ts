import { fileURLToPath } from 'node:url';

import { tanstackRouter } from '@tanstack/router-plugin/vite';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';
import { VitePWA } from 'vite-plugin-pwa';

// 開発では Vite が画面を出し、`/api` をゲートウェイへ中継する。ゲートウェイの `[api] public_url` を
// Vite の URL (http://localhost:5173) にすると、GitHub のログインのコールバックも Vite を通って届き、
// cookie と Origin の確認もそのまま働く。
const gateway = process.env.CHLOE_API ?? 'http://127.0.0.1:8080';

export default defineConfig({
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
  plugins: [
    // ルートは src/routes のファイルから作る (routeTree.gen.ts)。react() より前に置く。
    tanstackRouter({ target: 'react', autoCodeSplitting: true }),
    react(),
    VitePWA({
      // あとでプッシュ通知を足せるよう、Service Worker は自分で書く (src/sw.ts)。
      strategies: 'injectManifest',
      srcDir: 'src',
      filename: 'sw.ts',
      registerType: 'autoUpdate',
      injectRegister: false,
      manifest: {
        name: 'chloe',
        short_name: 'chloe',
        description: 'Answer your agents and watch your workspaces',
        start_url: '/',
        scope: '/',
        display: 'standalone',
        background_color: '#ffffff',
        theme_color: '#0ca678',
        icons: [
          { src: '/icons/icon-192.png', sizes: '192x192', type: 'image/png' },
          { src: '/icons/icon-512.png', sizes: '512x512', type: 'image/png' },
          {
            src: '/icons/maskable-512.png',
            sizes: '512x512',
            type: 'image/png',
            purpose: 'maskable',
          },
        ],
      },
      injectManifest: {
        globPatterns: ['**/*.{js,css,html,svg,png,woff2}'],
      },
    }),
  ],
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    css: false,
  },
  server: {
    proxy: {
      '/api': { target: gateway },
    },
  },
});
