import { type Page } from '@playwright/test';

import { expect, openTerminal, run, terminalText, test } from './fixtures';

// web-test.sh が作るワークスペース。alice は運用者 (admins) なので、acme のシークレットを変えられる。
const workspace = 'acme/proj';

// 端末で httpbin.org に送り、本物の値に差し替わったかを出す。印 (`mark`) のあとの
// 値を確かめる (打ったコマンドの行には `=$(` しか出ない)。
async function send(page: Page, mark: string) {
  await run(
    page,
    `echo "${mark}=$(curl -s -H "x-e2e-token: $E2E_TOKEN" https://httpbin.org/headers | grep -c real-e2e-value)"`,
  );
}

test('registers a secret from the web and the VM uses it', async ({ page, context }, testInfo) => {
  const name = `e2e_${testInfo.project.name}`;
  const secrets = await context.newPage();
  await secrets.goto('/workspaces');
  await secrets.getByRole('link', { name: 'Secrets' }).click();
  await secrets.getByRole('combobox', { name: 'Owner' }).click();
  await secrets.getByRole('option', { name: 'acme', exact: true }).click();
  await expect(secrets).toHaveURL(/owner=org%3Aacme/);

  await secrets.getByRole('button', { name: 'New secret' }).click();
  await secrets.getByRole('menuitem', { name: 'Blank' }).click();
  const dialog = secrets.getByRole('dialog');
  await dialog.getByRole('textbox', { name: 'Name', exact: true }).fill(name);
  const env = dialog.getByRole('combobox', { name: 'Environment variables' });
  await env.fill('E2E_TOKEN');
  await env.press('Enter');
  await dialog.getByLabel('Value of value').fill('real-e2e-value');
  const hosts = dialog.getByRole('combobox', { name: 'Hosts of header rule 1' });
  await hosts.fill('httpbin.org');
  await hosts.press('Enter');
  await dialog.getByRole('textbox', { name: 'Header' }).fill('x-e2e-token');
  await dialog.getByRole('button', { name: 'Save' }).click();
  await expect(secrets.getByText(`Saved ${name} for acme`)).toBeVisible();
  await expect(dialog).toBeHidden();
  await expect(secrets.getByText('Sent to httpbin.org')).toBeVisible();

  // 新しい種類のプレースホルダーは、このあとに開くシェルに入る。
  await openTerminal(page, workspace);
  await run(page, 'echo "ENV=${E2E_TOKEN:+set}"');
  await expect(terminalText(page)).toContainText('ENV=set');
  await send(page, 'SENT1');
  await expect(terminalText(page)).toContainText('SENT1=1');

  // 消すと差し替わらず、宛先も許されなくなる。
  await secrets.getByRole('button', { name: `Change ${name}` }).click();
  await secrets.getByRole('menuitem', { name: 'Remove' }).click();
  await secrets.getByRole('button', { name: 'Remove', exact: true }).click();
  await expect(secrets.getByText(`Removed ${name}`)).toBeVisible();
  await expect(secrets.getByText(name, { exact: true })).toBeHidden();
  await run(
    page,
    'echo "GONE=$(curl -s -o /dev/null -w "%{http_code}" https://httpbin.org/headers)"',
  );
  await expect(terminalText(page)).toContainText('GONE=403');
});
