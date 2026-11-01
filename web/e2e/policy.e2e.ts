import { expect, openTerminal, run, terminalText, test } from './fixtures';

// web-test.sh が作るワークスペース。
const workspace = 'acme/proj';

// VM の中から example.com に届くかを、状態のコードで確かめる。印 (n) で、前の出力と見分ける。
async function reach(page: Parameters<typeof run>[0], n: number) {
  await run(
    page,
    `echo "REACH${n}=$(curl -s -o /dev/null -w '%{http_code}' https://example.com/)"`,
  );
}

async function openPolicy(page: Parameters<typeof run>[0]) {
  await page.getByRole('button', { name: 'Workspace menu' }).click();
  await page.getByRole('menuitem', { name: 'Settings' }).click();
  const modal = page.getByRole('dialog');
  await modal.getByRole('tab', { name: 'Policy' }).click();
  await expect(modal.getByText("This workspace's rules")).toBeVisible();
  return modal;
}

test('allows a host from the policy settings while the shell keeps running', async ({ page }) => {
  await openTerminal(page, workspace);
  await reach(page, 1);
  await expect(terminalText(page)).toContainText('REACH1=403');

  const modal = await openPolicy(page);
  await modal.getByRole('button', { name: 'Add rule' }).click();
  const hosts = modal.getByRole('combobox', { name: 'Hosts of rule 1' });
  await hosts.fill('example.com');
  await hosts.press('Enter');
  await modal.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByText('Saved the rules of acme/proj')).toBeVisible();
  await expect(modal.getByRole('button', { name: 'Save' })).toBeDisabled();
  await page.keyboard.press('Escape');
  await expect(modal).toBeHidden();

  await page.locator('.xterm').click();
  await reach(page, 2);
  await expect(terminalText(page)).toContainText('REACH2=200');

  // ほかのテストのために、ワークスペースの規則を空に戻す。
  const again = await openPolicy(page);
  await again.getByRole('button', { name: 'Remove rule 1' }).click();
  await again.getByRole('button', { name: 'Save' }).click();
  // 保存が終わってから閉じる (終わる前に閉じると、保存していない変更を捨てるかを尋ねる)。
  await expect(again.getByRole('button', { name: 'Save' })).toBeDisabled();
  await expect(page.getByText('Saved the rules of acme/proj').last()).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(again).toBeHidden();
  await page.locator('.xterm').click();
  await reach(page, 3);
  await expect(terminalText(page)).toContainText('REACH3=403');
});
