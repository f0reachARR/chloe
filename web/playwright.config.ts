// 本物のゲートウェイを相手にする E2E。ゲートウェイと VM は images/guest/dev/web-test.sh が用意し、
// 画面の URL (CHLOE_E2E_URL) と、`chloe web login` の URL (CHLOE_E2E_LOGIN) を渡す。
import { defineConfig, devices } from '@playwright/test';

export const storageState = 'e2e/.auth/alice.json';

export default defineConfig({
  testDir: 'e2e',
  testMatch: /.*\.e2e\.ts/,
  // VM の起動を待つことがある。
  timeout: 180_000,
  expect: { timeout: 20_000 },
  // 同じワークスペースのシェルを使うので、並べない。
  workers: 1,
  outputDir: process.env.CHLOE_E2E_OUT ?? 'test-results',
  reporter: [['list'], ['./e2e/result-reporter.ts']],
  use: {
    baseURL: process.env.CHLOE_E2E_URL,
    trace: 'retain-on-failure',
    // CHLOE_E2E_SCREENSHOTS=1 なら、見た目を確かめるために、すべてのテストで撮る。
    screenshot: process.env.CHLOE_E2E_SCREENSHOTS ? 'on' : 'only-on-failure',
  },
  projects: [
    { name: 'login', testMatch: /login\.setup\.ts/ },
    {
      name: 'mobile',
      use: { ...devices['Pixel 7'], colorScheme: 'light', storageState },
      dependencies: ['login'],
    },
    {
      name: 'desktop',
      use: { viewport: { width: 1400, height: 900 }, colorScheme: 'dark', storageState },
      dependencies: ['login'],
    },
  ],
});
