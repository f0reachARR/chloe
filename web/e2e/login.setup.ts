// `chloe web login` の URL でログインし、cookie をほかのテストに渡す (URL は一度しか使えない)。
import { expect, test as setup } from '@playwright/test';

import { storageState } from '../playwright.config';

setup('sign in with a login link', async ({ page }) => {
  const link = process.env.CHLOE_E2E_LOGIN;
  expect(link, 'CHLOE_E2E_LOGIN is the URL from `chloe web login`').toBeTruthy();
  await page.goto(link!);
  await page.getByRole('button', { name: 'Sign in' }).click();
  await page.waitForURL((url) => !url.pathname.startsWith('/login'));
  await page.context().storageState({ path: storageState });
});
