import { expect, openTerminal, run, terminalText, test } from './fixtures';

// web-test.sh が作るワークスペース。alice は運用者 (admins) なので、acme の設定を変えられる。
const workspace = 'acme/proj';

async function reach(page: Parameters<typeof run>[0], n: number) {
  await run(page, `echo "ORG${n}=$(curl -s -o /dev/null -w '%{http_code}' https://example.com/)"`);
}

test('allows a host for the whole organization from Policies', async ({ page, context }) => {
  await openTerminal(page, workspace);
  await reach(page, 1);
  await expect(terminalText(page)).toContainText('ORG1=403');

  // 端末は開いたまま、別のタブの Policies で acme の設定を変える。
  const policies = await context.newPage();
  await policies.goto('/workspaces');
  await policies.getByRole('link', { name: 'Policies' }).click();
  await expect(policies.getByRole('link', { name: 'gateway' })).toBeVisible();
  await expect(policies.getByRole('link', { name: workspace })).toBeVisible();
  await policies.getByRole('link', { name: 'acme', exact: true }).click();
  await expect(policies).toHaveURL(/scope=org%3Aacme/);
  await policies.getByRole('button', { name: 'Add rule' }).click();
  const hosts = policies.getByRole('combobox', { name: 'Hosts of rule 1' });
  await hosts.fill('example.com');
  await hosts.press('Enter');
  await policies.getByRole('button', { name: 'Save' }).click();
  await expect(policies.getByText('Saved the settings of acme')).toBeVisible();

  await reach(page, 2);
  await expect(terminalText(page)).toContainText('ORG2=200');

  // ほかのテストのために戻す。
  await policies.getByRole('button', { name: 'Remove rule 1' }).click();
  await policies.getByRole('button', { name: 'Save' }).click();
  await expect(policies.getByText('This layer adds no rules of its own.')).toBeVisible();
  await expect(policies.getByRole('button', { name: 'Save' })).toBeDisabled();
  await reach(page, 3);
  await expect(terminalText(page)).toContainText('ORG3=403');
});

test('opens the policy settings of a workspace from Policies', async ({ page }) => {
  await page.goto('/policies');
  await page.getByRole('link', { name: workspace }).click();
  const modal = page.getByRole('dialog');
  await expect(modal.getByRole('tab', { name: 'Policy' })).toHaveAttribute('aria-selected', 'true');
  await expect(modal.getByText("This workspace's rules")).toBeVisible();
  await expect(page).toHaveURL(/\/policies\?.*section=policy/);
});
