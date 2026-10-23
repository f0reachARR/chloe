import { expect, openTerminal, run, terminalText, test } from './fixtures';

// web-test.sh が作るワークスペース。
const workspace = 'acme/proj';

test('opens the terminal from the workspace list', async ({ page }) => {
  await page.goto('/workspaces');
  await page.getByRole('link', { name: 'Terminal' }).first().click();
  await expect(page).toHaveURL(/\/terminal\?workspace=/);
  await expect(page.getByText('Connected')).toBeVisible({ timeout: 150_000 });
});

test('runs commands in the same shell as SSH', async ({ page }) => {
  await openTerminal(page, workspace);
  await run(page, 'echo "E2E-$((40+2)) $TERM $(whoami) $CHLOE_WORKSPACE"');
  await expect(terminalText(page)).toContainText(`E2E-42 xterm-256color user ${workspace}`);
  // 端末の大きさが、画面に合わせて伝わっている (80x24 の既定のままではない)。
  await run(page, 'echo "SIZE=$(stty size)"');
  await expect(terminalText(page)).not.toContainText('SIZE=24 80');
  await expect(terminalText(page)).toContainText(/SIZE=\d+ \d+/);
});

test('sends keys a phone keyboard lacks', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'the key row is for phones');
  await openTerminal(page, workspace);
  // Ctrl を押してから c で、動いているコマンドを止める。
  await run(page, 'sleep 300; echo NOT-INTERRUPTED');
  await page.getByRole('button', { name: 'Ctrl' }).click();
  await expect(page.getByRole('button', { name: 'Ctrl' })).toHaveAttribute('aria-pressed', 'true');
  await page.keyboard.type('c');
  await expect(terminalText(page)).toContainText('^C');
  await expect(page.getByRole('button', { name: 'Ctrl' })).toHaveAttribute('aria-pressed', 'false');
  // ↑ で、前のコマンドを呼び戻す。
  await run(page, 'echo FIRST-$((1+1))');
  await expect(terminalText(page)).toContainText('FIRST-2');
  await page.getByRole('button', { name: '↑' }).click();
  await page.keyboard.press('Enter');
  await expect(async () => {
    const text = (await terminalText(page).textContent()) ?? '';
    expect(text.split('FIRST-2').length - 1).toBeGreaterThanOrEqual(2);
  }).toPass();
});

test('shows the exit code and opens a new shell', async ({ page }) => {
  await openTerminal(page, workspace);
  await run(page, 'exit 3');
  await expect(page.getByText('Exited (3)')).toBeVisible();
  await page.getByRole('button', { name: 'New shell' }).click();
  await expect(page.getByText('Connected')).toBeVisible({ timeout: 60_000 });
});

test.describe(() => {
  // 断られた WebSocket の切り替えは、ブラウザがコンソールに出す。
  test.use({ allowedConsoleErrors: [/WebSocket connection to .* failed/] });

  test('explains a workspace it cannot open', async ({ page }) => {
    await page.goto('/terminal?workspace=nobody%2Fnothing');
    await expect(page.getByText('Failed')).toBeVisible();
    await expect(page.getByText(/Could not open the terminal/)).toBeVisible();
  });
});

test('answers an ask from the inbox while the agent waits in the terminal', async ({
  page,
  context,
}) => {
  await openTerminal(page, workspace);
  // VM の中のエージェントの代わりに、端末で質問を出して答えを待つ。答えは ANSWER= を付けて出し、
  // 打ったコマンドの行と見分ける。
  await run(
    page,
    'chloe-ask question "Ship it from the web?" --option ship-now --option later | sed "s/^/ANSWER=/"',
  );
  const inbox = await context.newPage();
  await inbox.goto('/inbox');
  await expect(inbox.getByText('Ship it from the web?')).toBeVisible();
  await inbox.getByRole('button', { name: 'ship-now' }).click();
  await expect(inbox.getByText('Ship it from the web?')).toBeHidden();
  await expect(terminalText(page)).toContainText('ANSWER=ship-now');
});
