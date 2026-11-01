import '@xterm/xterm/css/xterm.css';

import {
  ActionIcon,
  Badge,
  Box,
  Button,
  Group,
  Menu,
  Modal,
  ScrollArea,
  Stack,
  Text,
  useComputedColorScheme,
} from '@mantine/core';
import { notifications } from '@mantine/notifications';
import {
  IconArrowLeft,
  IconDotsVertical,
  IconRefresh,
  IconRotateClockwise,
  IconSettings,
} from '@tabler/icons-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Link } from '@tanstack/react-router';
import { FitAddon } from '@xterm/addon-fit';
import { type ITheme, Terminal } from '@xterm/xterm';
import { type ReactNode, type SyntheticEvent, useEffect, useRef, useState } from 'react';

import { api, errorMessage, invalidatePath } from '@/shared/api/client';
import { openOverlay } from '@/shared/overlays';

import {
  type TerminalConnection,
  type TerminalState,
  connectTerminal,
  terminalUrl,
} from './connection';
import { extraKeys, withCtrl } from './keys';

const themes: Record<'light' | 'dark', ITheme> = {
  light: {
    background: '#ffffff',
    foreground: '#1f2328',
    cursor: '#1f2328',
    selectionBackground: '#b4d5fe',
  },
  dark: {
    background: '#1a1b1e',
    foreground: '#d4d4d4',
    cursor: '#d4d4d4',
    selectionBackground: '#264f78',
  },
};

const fontFamily = 'ui-monospace, SFMono-Regular, Menlo, Consolas, "Noto Sans Mono", monospace';

function StatusBadge({ state, stopped }: { state: TerminalState; stopped: boolean }) {
  if (stopped) {
    return (
      <Badge color="gray" variant="light">
        Workspace stopped
      </Badge>
    );
  }
  switch (state.kind) {
    case 'connecting':
      return (
        <Badge color="gray" variant="light">
          Connecting
        </Badge>
      );
    case 'starting':
      return (
        <Badge color="yellow" variant="light">
          Starting workspace
        </Badge>
      );
    case 'running':
      return (
        <Badge color="teal" variant="dot">
          Connected
        </Badge>
      );
    case 'exited':
      return (
        <Badge color="gray" variant="light">
          Exited ({state.code})
        </Badge>
      );
    case 'failed':
      return (
        <Badge color="red" variant="light">
          Failed
        </Badge>
      );
    case 'closed':
      return (
        <Badge color="gray" variant="light">
          Disconnected
        </Badge>
      );
  }
}

// キーの列を押しても、端末からフォーカスを外さない (スマートフォンのキーボードを閉じない)。
const keepFocus = (event: SyntheticEvent) => event.preventDefault();

// スマートフォンでキーボードが出ると、見える高さ (visualViewport) が縮む。端末をその高さに収める。
function useVisibleHeight(): number | undefined {
  const [height, setHeight] = useState(() => window.visualViewport?.height);
  useEffect(() => {
    const viewport = window.visualViewport;
    if (!viewport) {
      return;
    }
    const update = () => setHeight(viewport.height);
    viewport.addEventListener('resize', update);
    return () => viewport.removeEventListener('resize', update);
  }, []);
  return height;
}

const isEnded = (state: TerminalState) =>
  state.kind === 'exited' || state.kind === 'failed' || state.kind === 'closed';

// 切れたあと、ワークスペースが止まったせいか (設定、CLI、止めるまでの時間のどれで止めても、
// ゲートウェイからは guest-agent との接続が切れた失敗として届く)。止める操作が終わるまで
// 動いていると返ることがあるので、切れてからしばらくは読み直す。
function useStoppedAfterEnd(workspace: string, endedAt: number | null): boolean {
  const info = useQuery({
    ...api.queryOptions('get', '/api/v1/workspaces/{name}', {
      params: { path: { name: workspace } },
    }),
    enabled: endedAt !== null,
    refetchInterval: (query) =>
      query.state.data?.running && endedAt && Date.now() - endedAt < 15_000 ? 2000 : false,
  });
  return endedAt !== null && info.data?.running === false;
}

// 止めてから新しいシェルを開く (VM の大きさなど、次の起動から効く設定を効かせる)。
function RestartDialog({
  workspace,
  opened,
  onClose,
  onRestarted,
}: {
  workspace: string;
  opened: boolean;
  onClose: () => void;
  onRestarted: () => void;
}) {
  const queryClient = useQueryClient();
  const stop = api.useMutation('patch', '/api/v1/workspaces/{name}', {
    onSuccess: onRestarted,
    onError: (error) =>
      notifications.show({ color: 'red', title: 'Could not stop', message: errorMessage(error) }),
    onSettled: () =>
      Promise.all([
        invalidatePath(queryClient, '/api/v1/workspaces'),
        invalidatePath(queryClient, '/api/v1/workspaces/{name}'),
      ]),
  });
  return (
    <Modal opened={opened} onClose={onClose} title="Restart workspace?">
      <Text size="sm" mb="md">
        This stops the VM, ending this shell and any running agents, then opens a new shell, which
        starts the VM again.
      </Text>
      <Group justify="flex-end" gap="xs">
        <Button variant="default" onClick={onClose}>
          Cancel
        </Button>
        <Button
          color="red"
          loading={stop.isPending}
          onClick={() =>
            stop.mutate({ params: { path: { name: workspace } }, body: { running: false } })
          }
        >
          Restart
        </Button>
      </Group>
    </Modal>
  );
}

// 一つのシェル。
function TerminalSession({
  workspace,
  actions,
  onRestart,
}: {
  workspace: string;
  actions?: ReactNode;
  onRestart: () => void;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const connectionRef = useRef<TerminalConnection | null>(null);
  const ctrlRef = useRef(false);
  const [ctrl, setCtrl] = useState(false);
  const [state, setState] = useState<TerminalState>({ kind: 'connecting' });
  // 切れた時刻 (止まったせいかを、それからしばらく確かめる)。
  const [endedAt, setEndedAt] = useState<number | null>(null);
  const scheme = useComputedColorScheme('dark');
  const schemeRef = useRef(scheme);
  const visibleHeight = useVisibleHeight();

  const armCtrl = (armed: boolean) => {
    ctrlRef.current = armed;
    setCtrl(armed);
  };

  // 端末とシェルは、ワークスペースが変わったときだけ作り直す (開き直すときは部品ごと作り直す)。
  useEffect(() => {
    const container = containerRef.current;
    if (!container) {
      return;
    }
    const terminal = new Terminal({
      fontFamily,
      fontSize: 14,
      cursorBlink: true,
      scrollback: 5000,
      theme: themes[schemeRef.current],
    });
    const fit = new FitAddon();
    terminal.loadAddon(fit);
    terminal.open(container);
    fit.fit();
    const connection = connectTerminal({
      url: terminalUrl(window.location.origin, workspace, terminal.cols, terminal.rows),
      onOutput: (data) => terminal.write(data),
      onState: (next) => {
        setState(next);
        if (isEnded(next)) {
          setEndedAt((prev) => prev ?? Date.now());
        }
      },
    });
    const subscriptions = [
      terminal.onData((data) => {
        if (ctrlRef.current) {
          ctrlRef.current = false;
          setCtrl(false);
          connection.send(withCtrl(data));
          return;
        }
        connection.send(data);
      }),
      // マウスの報告など、UTF-8 ではないバイト列。
      terminal.onBinary((data) =>
        connection.send(Uint8Array.from(data, (c) => c.charCodeAt(0) & 0xff)),
      ),
      terminal.onResize(({ cols, rows }) => connection.resize(cols, rows)),
    ];
    // 大きさが落ち着いてから合わせる (シェルに大きさの変更を何度も送らないように)。
    let timer: ReturnType<typeof setTimeout> | undefined;
    const observer = new ResizeObserver(() => {
      clearTimeout(timer);
      timer = setTimeout(() => fit.fit(), 100);
    });
    observer.observe(container);
    terminal.focus();
    terminalRef.current = terminal;
    connectionRef.current = connection;
    return () => {
      clearTimeout(timer);
      observer.disconnect();
      for (const subscription of subscriptions) {
        subscription.dispose();
      }
      connection.close();
      terminal.dispose();
      terminalRef.current = null;
      connectionRef.current = null;
    };
  }, [workspace, ctrlRef]);

  useEffect(() => {
    schemeRef.current = scheme;
    if (terminalRef.current) {
      terminalRef.current.options.theme = themes[scheme];
    }
  }, [scheme]);

  const sendKey = (data: string) => {
    armCtrl(false);
    connectionRef.current?.send(data);
    terminalRef.current?.focus();
  };

  const ended = isEnded(state);
  const stopped = useStoppedAfterEnd(workspace, endedAt);
  const [restarting, setRestarting] = useState(false);
  // 見える高さから、ヘッダー、下のナビゲーション、余白を引いた高さに収める。
  const height = `calc(${visibleHeight ? `${visibleHeight}px` : '100dvh'} - var(--app-shell-header-offset, 0px) - var(--app-shell-footer-offset, 0px) - 2 * var(--mantine-spacing-md))`;

  return (
    <Stack gap="xs" h={height} mih={240}>
      <Group justify="space-between" wrap="nowrap" gap="xs">
        <Group gap="xs" wrap="nowrap" miw={0}>
          <Button
            component={Link}
            to="/workspaces"
            variant="subtle"
            size="compact-sm"
            px={4}
            aria-label="Back to workspaces"
          >
            <IconArrowLeft size={16} />
          </Button>
          <Text fw={600} ff="monospace" truncate>
            {workspace}
          </Text>
          <StatusBadge state={state} stopped={stopped} />
        </Group>
        <Group gap="xs" wrap="nowrap">
          {ended && (
            <Button size="compact-sm" leftSection={<IconRefresh size={14} />} onClick={onRestart}>
              {state.kind === 'failed' && !stopped ? 'Retry' : 'New shell'}
            </Button>
          )}
          {actions}
          <Menu position="bottom-end" withinPortal>
            <Menu.Target>
              <ActionIcon variant="subtle" color="gray" aria-label="Workspace menu">
                <IconDotsVertical size={16} />
              </ActionIcon>
            </Menu.Target>
            <Menu.Dropdown>
              <Menu.Item
                leftSection={<IconSettings size={14} />}
                renderRoot={(props) => (
                  <Link to="." search={openOverlay('settings', workspace)} {...props} />
                )}
              >
                Settings
              </Menu.Item>
              <Menu.Item
                leftSection={<IconRotateClockwise size={14} />}
                onClick={() => setRestarting(true)}
              >
                Restart workspace
              </Menu.Item>
            </Menu.Dropdown>
          </Menu>
        </Group>
      </Group>
      <RestartDialog
        workspace={workspace}
        opened={restarting}
        onClose={() => {
          setRestarting(false);
          terminalRef.current?.focus();
        }}
        onRestarted={onRestart}
      />
      {state.kind === 'failed' && !stopped && (
        <Text size="sm" c="red">
          {state.message}
        </Text>
      )}
      <Box
        ref={containerRef}
        flex={1}
        mih={0}
        p={4}
        bg={themes[scheme].background}
        style={{
          border: '1px solid var(--mantine-color-default-border)',
          borderRadius: 'var(--mantine-radius-md)',
          overflow: 'hidden',
        }}
      />
      <ScrollArea type="never">
        <Group gap={6} wrap="nowrap">
          <Button
            size="compact-sm"
            variant={ctrl ? 'filled' : 'default'}
            onMouseDown={keepFocus}
            onClick={() => {
              armCtrl(!ctrl);
              terminalRef.current?.focus();
            }}
            aria-pressed={ctrl}
          >
            Ctrl
          </Button>
          {extraKeys.map((key) => (
            <Button
              key={key.label}
              size="compact-sm"
              variant="default"
              onMouseDown={keepFocus}
              onClick={() => sendKey(key.data)}
            >
              {key.label}
            </Button>
          ))}
        </Group>
      </ScrollArea>
    </Stack>
  );
}

// ワークスペースのシェル (SSH の `ssh -t` と同じ)。閉じるとシェルは終わる。開き直すと、新しい
// シェルを開く (切れたシェルにはつなぎ直せない)。
// `actions` はヘッダーに並べる、ほかの機能の操作 (ルートが組む)。
export function TerminalPage({ workspace, actions }: { workspace: string; actions?: ReactNode }) {
  const [attempt, setAttempt] = useState(0);
  return (
    <TerminalSession
      key={attempt}
      workspace={workspace}
      actions={actions}
      onRestart={() => setAttempt((n) => n + 1)}
    />
  );
}
