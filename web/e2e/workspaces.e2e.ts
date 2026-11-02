import { expect, terminalText, test } from './fixtures';

// 消すと端末が切れ、止まったかを確かめる問い合わせが 404 になり、ブラウザがコンソールに出す。
test.use({ allowedConsoleErrors: [/Failed to load resource: .* 404/] });

// web-test.sh は、alice を team acme/dev のメンバーにする (GitHub App がないので設定で)。
test('creates a team workspace, opens it and deletes it', async ({ page }, testInfo) => {
  // 二つの画面の幅で、別のワークスペースを使う。
  const name = `e2e-${testInfo.project.name}`;
  const full = `acme/dev/${name}`;
  await page.goto('/workspaces');
  await page.getByRole('button', { name: 'New workspace' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('combobox', { name: 'Owner' }).click();
  await page.getByRole('option', { name: 'acme/dev (team)' }).click();
  await dialog.getByRole('textbox', { name: 'Name' }).fill(name);
  await expect(dialog.getByText(full)).toBeVisible();
  // VM を小さくする (acme/proj と同時に動く)。
  await dialog.getByRole('button', { name: 'Advanced' }).click();
  await dialog.getByRole('textbox', { name: 'vCPUs' }).fill('1');
  await dialog.getByRole('textbox', { name: 'Memory (GiB)' }).fill('2');
  await dialog.getByRole('button', { name: 'Create' }).click();
  await expect(page.getByText(`Created ${full}`)).toBeVisible();
  await expect(dialog).toBeHidden();

  const card = page.locator('.mantine-Card-root', { hasText: full });
  await expect(card.getByText('Stopped')).toBeVisible();
  await expect(card.getByText(/1 vCPU · 2 GiB/)).toBeVisible();
  await card.getByRole('link', { name: 'Terminal' }).click();
  await expect(page.getByText('Connected')).toBeVisible({ timeout: 150_000 });
  await expect(terminalText(page)).toContainText('$', { timeout: 30_000 });

  // 端末から設定を開き、名前を打ち込んで消すと、一覧へ移る。
  await page.getByRole('button', { name: 'Workspace menu' }).click();
  await page.getByRole('menuitem', { name: 'Settings' }).click();
  const settings = page.getByRole('dialog');
  await settings.getByRole('tab', { name: 'Delete' }).click();
  const remove = settings.getByRole('button', { name: 'Delete workspace' });
  await expect(remove).toBeDisabled();
  await settings.getByRole('checkbox', { name: /Also delete the disks/ }).check();
  await settings.getByRole('textbox', { name: /to confirm/ }).fill(full);
  await remove.click();
  await expect(page.getByText(`Deleted ${full}`)).toBeVisible({ timeout: 60_000 });
  await expect(page).toHaveURL(/\/workspaces$/);
  await expect(page.getByText(full, { exact: true })).toBeHidden();
});
