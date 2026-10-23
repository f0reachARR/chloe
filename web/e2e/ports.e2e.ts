import type { BrowserContext, Page } from '@playwright/test';

import { expect, test } from './fixtures';

// web-test.sh が acme/proj の VM の中で 5173 を公開し、"hello from 5173" を返すサーバーを動かす。
const proxyHost = /^http:\/\/[a-z2-7]{12}\.preview\.localhost:\d+\//;

test('opens a published port through the web UI sign-in', async ({ page, context }) => {
  await page.goto('/workspaces');
  const link = page.getByRole('link', { name: /5173/ });
  await expect(link).toHaveAttribute('href', proxyHost);
  const [opened] = await Promise.all([context.waitForEvent('page'), link.click()]);
  // プロキシのホストは Web の画面でコードを作らせ、自分の cookie に替えてから中継する。
  await expect(opened.locator('body')).toContainText('hello from 5173');
  expect(opened.url()).toMatch(proxyHost);
  const cookies = await context.cookies(opened.url());
  expect(cookies.find((c) => c.name === 'chloe_proxy')).toMatchObject({ httpOnly: true });
  // Web の画面のセッションの cookie は、プロキシのホストには付かない。
  expect(cookies.find((c) => c.name === 'chloe_session')).toBeUndefined();
});

// ワークスペースのカードのリンクから、公開したポートを新しいタブで開く (そのホストにログインする)。
async function openPort(page: Page, context: BrowserContext, port: number) {
  const [opened] = await Promise.all([
    context.waitForEvent('page'),
    page.getByRole('link', { name: new RegExp(String(port)) }).click(),
  ]);
  await expect(opened.locator('body')).toContainText(`hello from ${port}`);
  return opened;
}

// ページの中で WebSocket を開き、送った文字列への返事を返す。開けなければ `refused`。
function echo(from: Page, url: string) {
  return from.evaluate(
    (target) =>
      new Promise<string>((resolve) => {
        const socket = new WebSocket(target);
        socket.addEventListener('open', () => socket.send('from-page'));
        socket.addEventListener('message', (event) => {
          resolve(String(event.data));
          socket.close();
        });
        socket.addEventListener('error', () => resolve('refused'));
      }),
    url,
  );
}

test('opens a WebSocket from the page but not from another port', async ({ page, context }) => {
  await page.goto('/workspaces');
  const own = await openPort(page, context, 5173);
  const other = await openPort(page, context, 5174);
  const url = new URL('/ws', own.url());
  url.protocol = 'ws:';
  // ブラウザは Sec-WebSocket-Accept を照合するので、切り替えの応答のヘッダーが届いていることも分かる。
  expect(await echo(own, url.href)).toBe('echo:from-page');
  // ほかのポートのページからは、5173 のホストの cookie が付いても断る (同じサイトなので付く)。
  expect(await echo(other, url.href)).toBe('refused');
});

test.describe(() => {
  // ログインしていないブラウザで開く。
  test.use({ storageState: { cookies: [], origins: [] } });

  test('signs in first and comes back to the page', async ({ page, isMobile }) => {
    test.skip(isMobile, 'uses the one login link for this test once');
    const link = process.env.CHLOE_E2E_LOGIN_PROXY;
    const url = process.env.CHLOE_E2E_PROXY_URL;
    expect(link && url, 'web-test.sh passes a second login link and the port URL').toBeTruthy();
    await page.goto(`${url}some/page?x=1`);
    await expect(page).toHaveURL(/\/login$/);
    // 届いた `chloe web login` の URL を、同じブラウザで開いたとみなす。
    await page.goto(link!);
    await page.getByRole('button', { name: 'Sign in' }).click();
    await expect(page.locator('body')).toContainText('hello from 5173');
    expect(page.url()).toBe(`${url}some/page?x=1`);
  });
});
