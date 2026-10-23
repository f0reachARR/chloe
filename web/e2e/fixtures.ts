// E2E の共通部分。コンソールの誤り (CSP の違反など) があれば、テストを落とす。誤りが出るはずの
// テストは、allowedConsoleErrors に許すものを書く。
import { type Page, expect, test as base } from '@playwright/test';

export const test = base.extend<{ allowedConsoleErrors: RegExp[]; consoleErrors: void }>({
  allowedConsoleErrors: [[], { option: true }],
  consoleErrors: [
    async ({ page, allowedConsoleErrors }, use) => {
      const errors: string[] = [];
      const record = (text: string) => {
        if (!allowedConsoleErrors.some((pattern) => pattern.test(text))) {
          errors.push(text);
        }
      };
      page.on('console', (message) => message.type() === 'error' && record(message.text()));
      page.on('pageerror', (error) => record(error.message));
      await use();
      expect(errors).toEqual([]);
    },
    { auto: true },
  ],
});

export { expect };

// 端末に描かれている文字。
export function terminalText(page: Page) {
  return page.locator('.xterm-rows');
}

// 端末を開き、シェルが起動するまで待つ。
export async function openTerminal(page: Page, workspace: string) {
  await page.goto(`/terminal?workspace=${encodeURIComponent(workspace)}`);
  await expect(page.getByText('Connected')).toBeVisible({ timeout: 150_000 });
  // プロンプトが出てから打つ。
  await expect(terminalText(page)).toContainText('$', { timeout: 30_000 });
  await page.locator('.xterm').click();
}

// 端末にコマンドを打つ。
export async function run(page: Page, command: string) {
  await page.keyboard.type(command);
  await page.keyboard.press('Enter');
}
