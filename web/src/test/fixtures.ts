import type { Schemas } from '@/shared/api/client';

const now = Math.floor(Date.now() / 1000);

export function networkAsk(overrides: Partial<Schemas['AskInfo']> = {}): Schemas['AskInfo'] {
  return {
    id: 1,
    workspace: 'acme/proj',
    kind: 'network',
    host: 'docs.example.com',
    methods: ['GET'],
    paths: ['/api/*'],
    reason: 'to read the API docs',
    status: 'pending',
    created_at: now - 300,
    expires_at: now + 86400,
    can_allow: true,
    ...overrides,
  } as Schemas['AskInfo'];
}

export function questionAsk(overrides: Partial<Schemas['AskInfo']> = {}): Schemas['AskInfo'] {
  return {
    id: 2,
    workspace: 'api',
    kind: 'question',
    question: 'Which branch?',
    options: ['main', 'release/1.2'],
    status: 'pending',
    created_at: now - 60,
    expires_at: now + 86400,
    can_allow: false,
    ...overrides,
  } as Schemas['AskInfo'];
}

export function workspace(
  overrides: Partial<Schemas['WorkspaceInfo']> = {},
): Schemas['WorkspaceInfo'] {
  return {
    name: 'default',
    root: 'ephemeral',
    running: true,
    workspace_disk_gib: 20,
    docker_disk_gib: 40,
    vcpus: 4,
    memory_mib: 4096,
    idle_timeout_secs: 1800,
    pending_asks: 0,
    busy: [],
    ...overrides,
  };
}
