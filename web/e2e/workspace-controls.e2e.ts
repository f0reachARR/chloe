import { expect, openTerminal, run, terminalText, test } from './fixtures';

// web-test.sh が作るワークスペース。
const workspace = 'acme/proj';

test('answers an ask over the terminal without leaving it', async ({ page }) => {
  await openTerminal(page, workspace);
  await run(page, 'export MARK=same-shell');
  await run(
    page,
    'chloe-ask question "Answer from the terminal?" --option yes-here --option no | sed "s/^/ANSWER=/"',
  );
  await page.getByRole('link', { name: '1 asks waiting' }).click();
  const drawer = page.getByRole('dialog');
  await expect(drawer.getByText('Answer from the terminal?')).toBeVisible();
  await drawer.getByRole('button', { name: 'yes-here' }).click();
  await expect(drawer.getByText('No asks are waiting for an answer.')).toBeVisible();
  await expect(terminalText(page)).toContainText('ANSWER=yes-here');
  await expect(page).toHaveURL(/\/terminal\?/);

  await page.keyboard.press('Escape');
  await expect(drawer).toBeHidden();
  await page.locator('.xterm').click();
  await run(page, 'echo "MARK=$MARK"');
  await expect(terminalText(page)).toContainText('MARK=same-shell');
  await expect(page.getByText('Connected')).toBeVisible();
});

test('changes settings over the terminal without ending the shell', async ({ page }) => {
  await openTerminal(page, workspace);
  await run(page, 'export MARK=kept');
  await page.getByRole('button', { name: 'Workspace menu' }).click();
  await page.getByRole('menuitem', { name: 'Settings' }).click();
  const modal = page.getByRole('dialog');
  await expect(modal.getByRole('tab', { name: 'Machine' })).toHaveAttribute(
    'aria-selected',
    'true',
  );
  const idle = modal.getByRole('textbox', { name: 'Idle time' });
  await idle.fill('45m');
  await modal.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText('Saved acme/proj')).toBeVisible();
  // 止めるまでの時間はすぐに効くので、止めるかは尋ねない。
  await expect(modal.getByText('Restart to apply')).toBeHidden();
  await modal.getByRole('tab', { name: 'Ports' }).click();
  await expect(page).toHaveURL(/section=ports/);

  await page.keyboard.press('Escape');
  await expect(modal).toBeHidden();
  await page.locator('.xterm').click();
  await run(page, 'echo "MARK=$MARK"');
  await expect(terminalText(page)).toContainText('MARK=kept');

  // ほかのテストのために、ゲートウェイの既定に戻す。
  await page.getByRole('button', { name: 'Workspace menu' }).click();
  await page.getByRole('menuitem', { name: 'Settings' }).click();
  await modal.getByRole('tab', { name: 'Machine' }).click();
  await modal.getByRole('combobox', { name: 'Idle stop' }).click();
  await page.getByRole('option', { name: "Gateway's default" }).click();
  await modal.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText('Saved acme/proj').last()).toBeVisible();
});

test('stops the workspace from the settings and starts it with a new shell', async ({
  page,
  isMobile,
}) => {
  // 止めると、web-test.sh が VM の中で動かした、公開したポートのサーバーも止まる。ほかのテストの
  // あとに一度だけ回るよう、最後に動く広い画面だけで確かめる。
  test.skip(isMobile, 'stopping ends the server behind the published port; run it once, last');
  await openTerminal(page, workspace);
  await page.getByRole('button', { name: 'Workspace menu' }).click();
  await page.getByRole('menuitem', { name: 'Settings' }).click();
  const modal = page.getByRole('dialog');
  await modal.getByRole('button', { name: 'Stop', exact: true }).click();
  await modal.getByRole('button', { name: 'Stop now' }).click();
  await expect(modal.getByText('Stopped', { exact: true })).toBeVisible({ timeout: 60_000 });
  await page.keyboard.press('Escape');

  // 失敗ではなく、止まったと出す。
  await expect(page.getByText('Workspace stopped')).toBeVisible();
  await expect(page.getByText(/guest-agent/)).toBeHidden();
  await page.getByRole('button', { name: 'New shell' }).click();
  await expect(page.getByText('Connected')).toBeVisible({ timeout: 150_000 });
  await expect(terminalText(page)).toContainText('$', { timeout: 30_000 });
});
